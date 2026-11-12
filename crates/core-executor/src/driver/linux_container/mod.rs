//! The `linux-container` driver: one container per instance, running
//! `needy daemon --mode executor` on a socket in a directory shared with the
//! host. Containers are created through the Docker-compatible engine API
//! (`bollard`), wherever `DOCKER_HOST` points.
//!
//! A profile sets run options as typed `container:` fields, which map to the
//! Docker API's container body one by one. The daemon owns some fields,
//! which are not profile fields, and refuses privileged ones unless the
//! operator allows them.
//!
//! NOTICE: the socket in a bind-mounted directory only works where the
//! container and the daemon share a kernel (Linux). Docker Desktop on macOS
//! runs containers in a VM, where the host cannot connect to it.

mod options;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use bollard::errors::Error as DockerError;
use bollard::query_parameters::{CreateContainerOptionsBuilder, CreateImageOptionsBuilder, RemoveContainerOptionsBuilder};
use futures_util::TryStreamExt;

use options::socket_dir;
pub use options::{Container, container};

use super::{Driver, Error, Instance, Spec, connect};

/// What the operator decides for every `linux-container` executor.
#[derive(Debug, Clone)]
pub struct Options {
  /// The image of an executor whose profile names none.
  pub image: String,
  /// The daemon's state directory; instance sockets and default workspaces
  /// live under it.
  pub state_dir: PathBuf,
  /// Privileged profile fields the operator allows: `privileged`,
  /// `cap_add`, `devices`, `gpus`, `pid`, `ipc`, `userns` and `uts` (their `host`
  /// mode), `security_opt` (an `unconfined` option), and `host_mounts` (a
  /// bind mount outside the executor's workspace).
  // TODO(operator): no operator configuration sets this yet, so privileged
  // options are always refused; it needs a daemon configuration file, which
  // has no slice yet.
  pub allow: Vec<String>,
}

/// Creates a host directory the container's user writes through a bind
/// mount: mode 0700, owned by that user. The directory's parents stay the
/// daemon's and private; a bind mount does not need them to be searchable.
// NOTICE: the container runs as the profile's `user`, which may differ from
// the daemon's (a daemon run as root, as on the cloud hosts of 2026-11-10).
// Only root can give a directory to another user; any other daemon fails at
// once with the reason instead of the container's executor failing to bind.
fn share_with_container(path: &Path, user: &needy_config::profile::User) -> Result<(), Error> {
  let failed = |source| Error::Directory {
    path: path.to_path_buf(),
    source,
  };
  #[cfg(unix)]
  {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(path).map_err(failed)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).map_err(failed)?;
    if nix::unistd::geteuid().as_raw() != user.uid {
      std::os::unix::fs::chown(path, Some(user.uid), Some(user.gid)).map_err(|source| Error::Ownership {
        path: path.to_path_buf(),
        uid: user.uid,
        gid: user.gid,
        source,
      })?;
    }
  }
  #[cfg(not(unix))]
  {
    let _ = user;
    std::fs::create_dir_all(path).map_err(failed)?;
  }
  Ok(())
}

/// The OCI runtimes this machine's Docker daemon (`DOCKER_HOST`, else its
/// default socket) has registered, such as `runc` and gVisor's `runsc`; an
/// error when it does not answer within `wait`.
pub async fn runtimes(wait: std::time::Duration) -> Result<BTreeSet<String>, Error> {
  let docker = bollard::Docker::connect_with_defaults()?.with_timeout(wait);
  let info = docker.info().await?;
  Ok(info.runtimes.unwrap_or_default().into_keys().collect())
}

/// The `linux-container` driver on this machine's Docker-compatible engine.
pub struct LinuxContainer {
  docker: bollard::Docker,
  options: Options,
}

impl LinuxContainer {
  /// Uses the local Docker daemon (`DOCKER_HOST`, else its default socket).
  /// Nothing is contacted until an instance is created.
  pub fn new(options: Options) -> Result<Self, Error> {
    Ok(Self {
      docker: bollard::Docker::connect_with_defaults()?,
      options,
    })
  }

  async fn create_container(&self, container: &Container) -> Result<(), Error> {
    let options = CreateContainerOptionsBuilder::default().name(&container.name).build();
    match self.docker.create_container(Some(options.clone()), container.body.clone()).await {
      Ok(_) => Ok(()),
      // Like `docker run`, pull an image that is not here yet.
      Err(DockerError::DockerResponseServerError {
        status_code: 404, ..
      }) => {
        let image = container.body.image.clone().unwrap_or_default();
        tracing::info!("pulling {image}");
        let pull = CreateImageOptionsBuilder::default().from_image(&with_tag(&image)).build();
        self.docker.create_image(Some(pull), None, None).try_collect::<Vec<_>>().await?;
        self.docker.create_container(Some(options), container.body.clone()).await?;
        Ok(())
      }
      Err(e) => Err(e.into()),
    }
  }

  /// Removes a container by name; one that is gone already is fine.
  /// Removes the container `name` and its socket's directory, as
  /// `destroy` does, without a handle: for a container a control daemon
  /// that restarted finds in its records. A container that is gone already
  /// is not an error.
  pub async fn remove_instance(&self, name: &str) -> Result<(), Error> {
    self.remove(name).await?;
    // The socket stays behind its container; the workspace is the user's.
    let dir = socket_dir(&self.options, name);
    if let Err(e) = std::fs::remove_dir_all(&dir)
      && e.kind() != std::io::ErrorKind::NotFound
    {
      tracing::warn!("cannot remove {}: {e}", dir.display());
    }
    Ok(())
  }

  async fn remove(&self, name: &str) -> Result<(), Error> {
    let options = RemoveContainerOptionsBuilder::default().force(true).build();
    match self.docker.remove_container(name, Some(options)).await {
      // 404: no such container; 409: its removal is in progress (`--rm`).
      Ok(())
      | Err(DockerError::DockerResponseServerError {
        status_code: 404 | 409,
        ..
      }) => Ok(()),
      Err(e) => Err(e.into()),
    }
  }
}

/// An image reference with its tag: the API pulls every tag of a bare name.
fn with_tag(image: &str) -> String {
  let last = image.rsplit('/').next().unwrap_or(image);
  if last.contains(':') || last.contains('@') {
    image.to_string()
  } else {
    format!("{image}:latest")
  }
}

#[async_trait::async_trait]
impl Driver for LinuxContainer {
  async fn create(&self, spec: &Spec) -> Result<Instance, Error> {
    let container = container(spec, &self.options)?;
    share_with_container(&container.socket_dir, &spec.user)?;
    // A workspace the profile names is the user's; the default one is the
    // daemon's, and the container writes it.
    if spec.config.workspace.is_some() {
      std::fs::create_dir_all(&container.workspace_dir).map_err(|source| Error::Directory {
        path: container.workspace_dir.clone(),
        source,
      })?;
    } else {
      share_with_container(&container.workspace_dir, &spec.user)?;
    }
    // A container left by an earlier daemon would block the name.
    self.remove(&container.name).await?;
    self.create_container(&container).await?;
    let ready = async {
      self.docker.start_container(&container.name, None).await?;
      connect(&format!("unix:{}", container.socket().display())).await
    };
    let client = match ready.await {
      Ok(client) => client,
      Err(error) => {
        if let Err(e) = self.remove(&container.name).await {
          tracing::warn!("cannot remove {} after a failed start: {e}", container.name);
        }
        return Err(error);
      }
    };
    Ok(Instance {
      driver: "linux-container",
      id: container.name,
      executor: std::sync::Arc::new(client),
      sftp: vec!["/usr/lib/openssh/sftp-server".into()],
      workspace: container.workspace,
    })
  }

  async fn destroy(&self, handle: &Instance) -> Result<(), Error> {
    self.remove_instance(&handle.id).await
  }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
