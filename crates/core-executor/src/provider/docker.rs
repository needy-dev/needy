//! The `docker` provider: one container per instance, running
//! `needy daemon --mode executor` on a socket in a directory shared with the
//! host. Containers are created through the Docker API (`bollard`).
//!
//! A profile sets run options as typed `docker:` fields, which map to the
//! Docker API's container body one by one. The daemon owns some fields,
//! which are not profile fields, and refuses privileged ones unless the
//! operator allows them.
//!
//! NOTICE: the socket in a bind-mounted directory only works where the
//! container and the daemon share a kernel (Linux). Docker Desktop on macOS
//! runs containers in a VM, where the host cannot connect to it.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use bollard::errors::Error as DockerError;
use bollard::models::{
  ContainerCreateBody, DeviceMapping, DeviceRequest, HostConfig, Mount, MountTmpfsOptions, MountType, ResourcesUlimits,
};
use bollard::query_parameters::{CreateContainerOptionsBuilder, CreateImageOptionsBuilder, RemoveContainerOptionsBuilder};
use futures_util::TryStreamExt;
use needy_config::profile::{Device, Docker as DockerConfig, Gpus, MemorySwap, Mount as ProfileMount, Ulimit};
use needy_config::value::ByteSize;

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
  /// Privileged profile fields the operator allows: `privileged`,
  /// `cap_add`, `devices`, `gpus`, `pid`, `ipc`, `userns` and `uts` (their `host`
  /// mode), `security_opt` (an `unconfined` option), and `host_mounts` (a
  /// bind mount outside the executor's workspace).
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
/// fields first, then the profile's typed `docker:` fields under the
/// operator's policy.
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
  let body = ContainerCreateBody {
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
    labels: Some(HashMap::from([("needy.run".to_string(), spec.run.clone())])),
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
  let mut container = Container {
    name,
    body,
    socket_dir,
    workspace_dir,
    workspace,
  };
  apply_run_options(&docker, &spec.base, &options.allow, &mut container)?;
  Ok(container)
}

/// Sets a profile's typed `docker:` fields on the container's body. The
/// daemon's own fields are not profile fields, so nothing here overrides
/// them; privileged fields need the operator.
///
/// Each field is the Docker API field that the matching `docker run` flag
/// sets, with the value `docker run` would send.
// Source: https://github.com/docker/cli/blob/v29.3.0/cli/command/container/opts.go#L604-L722
// (`docker run` flags to `Config`, `HostConfig` and its `Resources`), and
// https://github.com/docker/cli/blob/v29.3.0/opts/gpus.go#L36-L99 (`--gpus`).
// Reference: https://github.com/moby/moby/blob/v28.5.1/api/swagger.yaml#L917-L1195
// (Engine API 1.51 `HostConfig`); bollard's `HostConfig`, which flattens
// `Resources` into it, and `ContainerCreateBody`:
// https://docs.rs/crate/bollard-stubs/1.53.1-rc.29.3.1/source/src/models.rs#3402-3745
// and #1211-1341.
fn apply_run_options(docker: &DockerConfig, base: &Path, allow: &[String], container: &mut Container) -> Result<(), Error> {
  let operator = |name: &str, what: String| {
    if allow.iter().any(|a| a == name) {
      Ok(())
    } else {
      Err(Error::Config(format!("{what} is not allowed by the operator")))
    }
  };
  let body = &mut container.body;
  let host = body.host_config.get_or_insert_with(HostConfig::default);

  // Resources.
  host.nano_cpus = docker.cpus.map(|cpus| cpus.nanos() as i64);
  host.cpu_shares = docker.cpu_shares;
  host.cpuset_cpus = docker.cpuset_cpus.clone();
  host.memory = docker.memory.map(bytes).transpose()?;
  host.memory_swap = docker
    .memory_swap
    .map(|swap| match swap {
      MemorySwap::Limit(size) => bytes(size),
      // `docker run --memory-swap -1`.
      MemorySwap::Unlimited => Ok(-1),
    })
    .transpose()?;
  host.shm_size = docker.shm_size.map(bytes).transpose()?;
  host.pids_limit = docker.pids_limit;
  host.ulimits = some(
    docker
      .ulimits
      .iter()
      .map(|(name, limit)| {
        let (soft, hard) = match *limit {
          Ulimit::Same(both) => (both, both),
          Ulimit::Pair { soft, hard } => (soft, hard),
        };
        ResourcesUlimits {
          name: Some(name.clone()),
          soft: Some(soft),
          hard: Some(hard),
        }
      })
      .collect(),
  );
  // GPUs are host devices, so they need the operator as `devices` do
  // (owner decision, 2026-11-12), although Docker grants them without
  // `--privileged`.
  if let Some(gpus) = &docker.gpus {
    let what = match gpus {
      Gpus::All => "all".to_string(),
      Gpus::Count(count) => count.to_string(),
      Gpus::Devices(ids) => ids.join(", "),
    };
    operator("gpus", format!("gpus {what}"))?;
    host.device_requests = Some(vec![gpu_request(gpus)]);
  }

  // The process and its names.
  body.env = some(docker.env.iter().map(|(name, value)| format!("{name}={value}")).collect());
  if let Some(workdir) = &docker.workdir {
    body.working_dir = Some(absolute("workdir", workdir)?.to_string());
  }
  host.readonly_rootfs = docker.read_only;
  host.init = docker.init;
  // `HostConfig.ExtraHosts` entries are `name:address`.
  host.extra_hosts = some(docker.add_host.iter().map(|(name, address)| format!("{name}:{address}")).collect());
  host.dns = some(docker.dns.iter().map(ToString::to_string).collect());
  host.sysctls = (!docker.sysctls.is_empty()).then(|| docker.sysctls.clone().into_iter().collect());
  host.cap_drop = some(docker.cap_drop.clone());
  host.security_opt = some(docker.security_opt.clone());
  for option in &docker.security_opt {
    if option.contains("unconfined") {
      operator("security_opt", format!("security_opt {option}"))?;
    }
  }

  // Namespaces: another container's is fine, the host's needs the operator.
  for (name, mode, field) in [
    ("pid", &docker.pid, &mut host.pid_mode),
    ("ipc", &docker.ipc, &mut host.ipc_mode),
    ("userns", &docker.userns, &mut host.userns_mode),
    ("uts", &docker.uts, &mut host.uts_mode),
  ] {
    if mode.as_deref() == Some("host") {
      operator(name, format!("{name} host"))?;
    }
    *field = mode.clone();
  }

  // Privileges.
  if docker.privileged {
    operator("privileged", "privileged".into())?;
    host.privileged = Some(true);
  }
  if !docker.cap_add.is_empty() {
    operator("cap_add", format!("cap_add {}", docker.cap_add.join(", ")))?;
    host.cap_add = Some(docker.cap_add.clone());
  }
  let mut devices = Vec::new();
  for device in &docker.devices {
    operator("devices", format!("devices {}", device.host))?;
    devices.push(device_mapping(device)?);
  }
  host.devices = some(devices);

  // Mounts.
  let mut mounts = Vec::new();
  for mount in &docker.mounts {
    let (typ, source, target, read_only, tmpfs_options) = match mount {
      ProfileMount::Bind {
        source,
        target,
        read_only,
      } => {
        let source = host_path(source, base);
        if !inside(&source, &container.workspace_dir) {
          operator("host_mounts", format!("the bind mount of {} is outside the executor's workspace and", source.display()))?;
        }
        (MountType::BIND, Some(source.display().to_string()), target, Some(*read_only), None)
      }
      ProfileMount::Volume {
        source,
        target,
        read_only,
      } => (MountType::VOLUME, source.clone(), target, Some(*read_only), None),
      ProfileMount::Tmpfs { target, size } => {
        let options = match size {
          Some(size) => Some(MountTmpfsOptions {
            size_bytes: Some(bytes(*size)?),
            ..Default::default()
          }),
          None => None,
        };
        (MountType::TMPFS, None, target, None, options)
      }
    };
    let target = absolute("mounts", target)?;
    for (daemons, what) in [
      (container.workspace.as_str(), "the workspace"),
      (SOCKET_DIR, "the executor's socket directory"),
    ] {
      if Path::new(target).starts_with(daemons) {
        return Err(Error::Config(format!("the mount at {target} is over {what}, which the daemon mounts")));
      }
    }
    mounts.push(Mount {
      typ: Some(typ),
      source,
      target: Some(target.to_string()),
      read_only,
      tmpfs_options,
      ..Default::default()
    });
  }
  host.mounts = some(mounts);
  Ok(())
}

/// `None` for an empty list, so that an absent field leaves Docker's default.
fn some<T>(values: Vec<T>) -> Option<Vec<T>> {
  (!values.is_empty()).then_some(values)
}

/// A size in the Docker API's signed bytes.
fn bytes(size: ByteSize) -> Result<i64, Error> {
  i64::try_from(size.bytes()).map_err(|_| Error::Config(format!("{} bytes is too large for Docker", size.bytes())))
}

/// A path in the container, which is a Linux path whatever the host is.
fn absolute<'a>(field: &str, path: &'a str) -> Result<&'a str, Error> {
  if path.starts_with('/') {
    Ok(path)
  } else {
    Err(Error::Config(format!("{field}: {path} is not an absolute path")))
  }
}

/// Whether the host path `path` is `dir` or under it, by its components: a
/// path with `..` is never inside.
// NOTICE: lexical only; a symbolic link inside the workspace can still point
// out of it. The workspace is the profile owner's own directory, so a link
// there reaches nothing its owner cannot reach already.
fn inside(path: &Path, dir: &Path) -> bool {
  !path.components().any(|c| c == std::path::Component::ParentDir) && path.starts_with(dir)
}

/// The device request `docker run --gpus` sends: every GPU (`all`, a count
/// of -1), a number of GPUs, or the GPUs with these IDs, with the `gpu`
/// capability and the runtime's default driver.
// Source: https://github.com/docker/cli/blob/v29.3.0/opts/gpus.go#L18-L31
// (`all` is a count of -1) and #L87-L95 (one GPU without a count or IDs,
// the `gpu` capability by default).
fn gpu_request(gpus: &Gpus) -> DeviceRequest {
  let (count, device_ids) = match gpus {
    Gpus::All => (Some(-1), None),
    Gpus::Count(n) => (Some(i64::from(*n)), None),
    Gpus::Devices(ids) => (None, Some(ids.clone())),
  };
  DeviceRequest {
    count,
    device_ids,
    capabilities: Some(vec![vec!["gpu".to_string()]]),
    ..Default::default()
  }
}

/// A host device in the container: at the host's path and with every
/// permission (`rwm`) unless the profile says otherwise, as `docker run
/// --device` does.
// Source: https://github.com/docker/cli/blob/v28.5.1/cli/command/container/opts.go#L999-L1033
// (`parseLinuxDevice`: the container path defaults to the host's, the
// permissions to `rwm`) and #L1053-L1071 (`validDeviceMode`: some of `r`,
// `w` and `m`, each at most once).
fn device_mapping(device: &Device) -> Result<DeviceMapping, Error> {
  let permissions = device.permissions.as_deref().unwrap_or("rwm");
  let unique = permissions.chars().collect::<BTreeSet<_>>().len() == permissions.len();
  if permissions.is_empty() || !unique || !permissions.chars().all(|c| "rwm".contains(c)) {
    return Err(Error::Config(format!("devices {}: bad permissions {permissions}: use some of r, w and m", device.host)));
  }
  let in_container = device.container.as_deref().unwrap_or(&device.host);
  absolute("devices", in_container)?;
  Ok(DeviceMapping {
    path_on_host: Some(device.host.clone()),
    path_in_container: Some(in_container.to_string()),
    cgroup_permissions: Some(permissions.to_string()),
  })
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
      executor: std::sync::Arc::new(client),
      sftp: vec!["/usr/lib/openssh/sftp-server".into()],
      workspace: container.workspace,
    })
  }

  async fn destroy(&self, handle: &InstanceHandle) -> Result<(), Error> {
    self.remove_instance(&handle.id).await
  }
}

#[cfg(test)]
#[path = "docker_test.rs"]
mod tests;
