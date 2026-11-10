//! The `docker` runtime: one container per instance, running
//! `needy daemon --mode executor` on a socket in a directory shared with the
//! host. Containers are created through the Docker API (`bollard`).
//!
//! A profile passes `docker run` options in `docker.args`. The daemon owns
//! some fields and refuses privileged ones unless the operator allows them;
//! the rest are translated to the Docker API's container body.
//!
//! NOTICE: the socket in a bind-mounted directory only works where the
//! container and the daemon share a kernel (Linux). Docker Desktop on macOS
//! runs containers in a VM, where the host cannot connect to it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use bollard::errors::Error as DockerError;
use bollard::models::{ContainerCreateBody, DeviceMapping, HostConfig, Mount, MountType, ResourcesUlimits};
use bollard::query_parameters::{CreateContainerOptionsBuilder, CreateImageOptionsBuilder, RemoveContainerOptionsBuilder};
use futures_util::TryStreamExt;

use super::{Error, InstanceHandle, InstanceSpec, Provider, connect, host_path, sanitize};

/// Where the instance's socket directory is mounted in the container.
const SOCKET_DIR: &str = "/run/needy";

/// What the operator decides for every Docker executor.
#[derive(Debug, Clone)]
pub struct Options {
  /// The image of an executor whose profile names none.
  pub image: String,
  /// The daemon's state directory; instance sockets and default workspaces
  /// live under it.
  pub state_dir: PathBuf,
  /// Privileged options the operator allows: `--privileged`, `--cap-add`,
  /// `--device`, `--pid`, `--ipc`, `--userns`, `--uts`, `--security-opt`,
  /// and `host-root` for mounting `/`.
  // TODO(operator): no operator configuration sets this yet, so privileged
  // options are always refused; it needs a daemon configuration file, which
  // has no slice yet.
  pub allow: Vec<String>,
}

/// A container the provider is about to create.
#[derive(Debug, Clone, PartialEq)]
pub struct Container {
  pub name: String,
  pub body: ContainerCreateBody,
  /// Host directory with the executor's socket.
  pub socket_dir: PathBuf,
  /// Host directory mounted as the workspace.
  pub workspace_dir: PathBuf,
  /// The workspace inside the container.
  pub workspace: String,
}

impl Container {
  fn socket(&self) -> PathBuf {
    self.socket_dir.join(SOCKET)
  }
}

/// The executor's socket in its socket directory.
const SOCKET: &str = "executor.sock";

/// The host directory with the socket of the container `name`: twelve hex
/// digits derived from the name, so a long key cannot make the socket's
/// path too long.
fn socket_dir(options: &Options, name: &str) -> PathBuf {
  let id = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, name.as_bytes()).simple().to_string();
  options.state_dir.join("executors").join(&id[..12])
}

/// Refuses a socket path that does not fit a Unix socket address
/// (`sun_path`), which the container's executor could not bind.
fn check_socket(path: &Path) -> Result<(), Error> {
  #[cfg(unix)]
  if nix::sys::socket::UnixAddr::new(path).is_err() {
    return Err(Error::Config(format!(
      "the executor's socket path {} is too long for a Unix socket; use a shorter state directory",
      path.display()
    )));
  }
  #[cfg(not(unix))]
  let _ = path;
  Ok(())
}

/// The container for one instance of a profile's executor: the daemon's
/// fields first, then the profile's `docker.args` under the flag policy.
pub fn container(spec: &InstanceSpec, options: &Options) -> Result<Container, Error> {
  let docker = spec.config.docker.clone().unwrap_or_default();
  let key = sanitize(&spec.key);
  let name = format!("needy-{key}");
  let socket_dir = socket_dir(options, &name);
  check_socket(&socket_dir.join(SOCKET))?;
  let workspace_dir = match &spec.config.workspace {
    Some(path) => host_path(path, &spec.base),
    None => options.state_dir.join("workspaces").join(&key),
  };
  // Paths inside the container are Linux paths, whatever the host is.
  let workspace = format!("/home/{}/workspace", spec.user.name);
  let mut body = ContainerCreateBody {
    image: Some(docker.image.clone().unwrap_or_else(|| options.image.clone())),
    cmd: Some(
      [
        "needy",
        "daemon",
        "--mode",
        "executor",
        "--listen",
        &format!("unix:{SOCKET_DIR}/{SOCKET}"),
        "--name",
        &spec.name,
      ]
      .map(String::from)
      .to_vec(),
    ),
    user: Some(format!("{}:{}", spec.user.uid, spec.user.gid)),
    hostname: spec.hostname.clone(),
    labels: Some(HashMap::from([("needy.session".to_string(), spec.session.clone())])),
    host_config: Some(HostConfig {
      auto_remove: Some(true),
      binds: Some(vec![
        format!("{}:{workspace}", workspace_dir.display()),
        format!("{}:{SOCKET_DIR}", socket_dir.display()),
      ]),
      ..Default::default()
    }),
    ..Default::default()
  };
  apply_run_options(&docker.args, &options.allow, &mut body)?;
  Ok(Container {
    name,
    body,
    socket_dir,
    workspace_dir,
    workspace,
  })
}

/// Options the daemon sets; a profile never overrides them.
const DAEMON_OWNED: &[&str] = &[
  "--network",
  "--net",
  "--name",
  "--label",
  "-l",
  "--hostname",
  "-h",
  "--user",
  "-u",
  "--entrypoint",
  "--rm",
  "--detach",
  "-d",
];

/// Options without a value.
const SWITCHES: &[&str] = &["--privileged", "--read-only", "--init"];

/// Applies `docker run` options from a profile to `body`.
///
/// NOTICE: no maintained crate translates `docker run` flags to the Docker
/// API's body (the Docker CLI does it in Go, in `cli/command/container/opts.go`),
/// so the options profiles use are translated here. An option outside this
/// list is an error rather than silently dropped.
// Source: https://github.com/docker/cli/blob/v28.5.1/cli/command/container/opts.go#L600-L712
// (flags to `HostConfig`), sizes with go-units' binary `RAMInBytes`,
// https://github.com/docker/go-units/blob/v0.5.0/size.go#L86-L88.
// Reference: https://github.com/moby/moby/blob/v28.5.1/api/swagger.yaml#L917-L1195
// (Engine API 1.51 `HostConfig`).
// TODO(docker): `--gpus`, `--add-host`, `--dns`, `--workdir`, `--cpu-shares`
// and `--sysctl` are not translated yet; add them when a profile needs them.
fn apply_run_options(args: &[String], allow: &[String], body: &mut ContainerCreateBody) -> Result<(), Error> {
  let allowed = |name: &str| allow.iter().any(|a| a == name);
  let refuse = |message: String| Err(Error::Config(message));
  let mut env = Vec::new();
  let host = body.host_config.get_or_insert_with(HostConfig::default);
  let mut args = args.iter();
  while let Some(arg) = args.next() {
    let (flag, inline) = match arg.split_once('=') {
      Some((flag, value)) if flag.starts_with('-') => (flag, Some(value.to_string())),
      _ => (arg.as_str(), None),
    };
    if DAEMON_OWNED.contains(&flag) {
      return refuse(format!("{flag} is set by the daemon"));
    }
    if SWITCHES.contains(&flag) {
      let on = match inline.as_deref() {
        None | Some("true") => true,
        Some("false") => false,
        Some(other) => return refuse(format!("{flag}={other}: expected true or false")),
      };
      match flag {
        "--privileged" if on && !allowed(flag) => return refuse(format!("{flag} is not allowed by the operator")),
        "--privileged" => host.privileged = Some(on),
        "--read-only" => host.readonly_rootfs = Some(on),
        _ => host.init = Some(on),
      }
      continue;
    }
    let value = match inline {
      Some(value) => value,
      None => args.next().cloned().ok_or_else(|| Error::Config(format!("{flag} needs a value")))?,
    };
    match flag {
      "--cpus" => {
        let cpus: f64 = value.parse().map_err(|_| Error::Config(format!("{flag} {value}: expected a number")))?;
        host.nano_cpus = Some((cpus * 1e9).round() as i64);
      }
      "--memory" | "-m" => host.memory = Some(size(flag, &value)?),
      "--memory-swap" => {
        host.memory_swap = Some(if value == "-1" {
          -1
        } else {
          size(flag, &value)?
        })
      }
      "--shm-size" => host.shm_size = Some(size(flag, &value)?),
      "--pids-limit" => host.pids_limit = Some(value.parse().map_err(|_| Error::Config(format!("{flag} {value}: expected a number")))?),
      "--cpuset-cpus" => host.cpuset_cpus = Some(value),
      "--env" | "-e" => {
        if !value.contains('=') {
          return refuse(format!("{flag} {value}: expected NAME=VALUE"));
        }
        env.push(value);
      }
      "--volume" | "-v" => {
        let mut parts = value.split(':');
        let source = parts.next().unwrap_or_default();
        let target = parts.next().unwrap_or_default();
        check_mount(source, target, &allowed)?;
        host.binds.get_or_insert_with(Vec::new).push(value);
      }
      "--mount" => {
        let mount = parse_mount(&value)?;
        check_mount(mount.source.as_deref().unwrap_or_default(), mount.target.as_deref().unwrap_or_default(), &allowed)?;
        host.mounts.get_or_insert_with(Vec::new).push(mount);
      }
      "--tmpfs" => {
        let (path, options) = value.split_once(':').unwrap_or((&value, ""));
        check_mount("", path, &allowed)?;
        host.tmpfs.get_or_insert_with(HashMap::new).insert(path.to_string(), options.to_string());
      }
      "--cap-add" | "--device" if !allowed(flag) => return refuse(format!("{flag} is not allowed by the operator")),
      "--cap-add" => host.cap_add.get_or_insert_with(Vec::new).push(value),
      "--cap-drop" => host.cap_drop.get_or_insert_with(Vec::new).push(value),
      // REVIEW: Docker reads a second field that is a valid mode (`/dev/x:rw`)
      // as the permissions, with the container path equal to the host's
      // (https://github.com/docker/cli/blob/v28.5.1/cli/command/container/opts.go#L1005-L1016);
      // here it always becomes the container path.
      "--device" => {
        let mut parts = value.splitn(3, ':');
        let on_host = parts.next().unwrap_or_default().to_string();
        let in_container = parts.next().map(String::from).unwrap_or_else(|| on_host.clone());
        host.devices.get_or_insert_with(Vec::new).push(DeviceMapping {
          path_on_host: Some(on_host),
          path_in_container: Some(in_container),
          cgroup_permissions: Some(parts.next().unwrap_or("rwm").to_string()),
        });
      }
      "--pid" | "--ipc" | "--userns" | "--uts" if value == "host" && !allowed(flag) => {
        return refuse(format!("{flag} host is not allowed by the operator"));
      }
      "--pid" => host.pid_mode = Some(value),
      "--ipc" => host.ipc_mode = Some(value),
      "--userns" => host.userns_mode = Some(value),
      "--uts" => host.uts_mode = Some(value),
      "--security-opt" if value.contains("unconfined") && !allowed(flag) => {
        return refuse(format!("{flag} {value} is not allowed by the operator"));
      }
      "--security-opt" => host.security_opt.get_or_insert_with(Vec::new).push(value),
      "--ulimit" => host.ulimits.get_or_insert_with(Vec::new).push(ulimit(&value)?),
      _ => return refuse(format!("{flag} is not a supported docker run option")),
    }
  }
  if !env.is_empty() {
    body.env.get_or_insert_with(Vec::new).extend(env);
  }
  Ok(())
}

/// A size as `docker run` takes it: binary units, `8g` is 8 GiB.
fn size(flag: &str, value: &str) -> Result<i64, Error> {
  parse_size::Config::new()
    .with_binary()
    .parse_size(value)
    .ok()
    .and_then(|bytes| i64::try_from(bytes).ok())
    .ok_or_else(|| Error::Config(format!("{flag} {value}: expected a size such as 512m or 8g")))
}

/// `NAME=SOFT[:HARD]`.
fn ulimit(value: &str) -> Result<ResourcesUlimits, Error> {
  let invalid = || Error::Config(format!("--ulimit {value}: expected NAME=SOFT[:HARD]"));
  let (name, limits) = value.split_once('=').ok_or_else(invalid)?;
  let (soft, hard) = limits.split_once(':').unwrap_or((limits, limits));
  Ok(ResourcesUlimits {
    name: Some(name.to_string()),
    soft: Some(soft.parse().map_err(|_| invalid())?),
    hard: Some(hard.parse().map_err(|_| invalid())?),
  })
}

/// `--mount type=bind,source=/a,target=/b,readonly`.
fn parse_mount(value: &str) -> Result<Mount, Error> {
  let mut mount = Mount {
    typ: Some(MountType::VOLUME),
    ..Default::default()
  };
  for pair in value.split(',') {
    let (key, field) = pair.split_once('=').unwrap_or((pair, "true"));
    match key {
      "type" => {
        mount.typ = Some(match field {
          "bind" => MountType::BIND,
          "volume" => MountType::VOLUME,
          "tmpfs" => MountType::TMPFS,
          other => return Err(Error::Config(format!("--mount type={other} is not supported; use bind, volume or tmpfs"))),
        });
      }
      "source" | "src" => mount.source = Some(field.to_string()),
      "target" | "dst" | "destination" => mount.target = Some(field.to_string()),
      "readonly" | "ro" => mount.read_only = Some(field == "true" || field == "1"),
      other => return Err(Error::Config(format!("--mount {other} is not supported"))),
    }
  }
  Ok(mount)
}

fn check_mount(source: &str, target: &str, allowed: &dyn Fn(&str) -> bool) -> Result<(), Error> {
  if target.split('/').any(|part| part == "workspace") {
    return Err(Error::Config(format!("the workspace mount at {target} is set by the daemon")));
  }
  if source == "/" && !allowed("host-root") {
    return Err(Error::Config("mounting the host root is not allowed by the operator".into()));
  }
  Ok(())
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

/// The Docker runtime on this machine's Docker daemon.
pub struct Docker {
  docker: bollard::Docker,
  options: Options,
}

impl Docker {
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
impl Provider for Docker {
  async fn create(&self, spec: &InstanceSpec) -> Result<InstanceHandle, Error> {
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
    Ok(InstanceHandle {
      runtime: "docker",
      id: container.name,
      client,
      sftp: vec!["/usr/lib/openssh/sftp-server".into()],
      workspace: container.workspace,
    })
  }

  async fn destroy(&self, handle: &InstanceHandle) -> Result<(), Error> {
    self.remove(&handle.id).await?;
    // The socket stays behind its container; the workspace is the user's.
    let dir = socket_dir(&self.options, &handle.id);
    if let Err(e) = std::fs::remove_dir_all(&dir)
      && e.kind() != std::io::ErrorKind::NotFound
    {
      tracing::warn!("cannot remove {}: {e}", dir.display());
    }
    Ok(())
  }
}

#[cfg(test)]
#[path = "docker_test.rs"]
mod tests;
