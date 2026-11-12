//! The run-option policy of a `linux-container` executor: the container
//! body for one instance, with the daemon's own fields first and then the
//! profile's typed `container:` fields under the operator's policy, each one
//! engine API field.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::engine::models::{
  ContainerCreateBody, DeviceMapping, DeviceRequest, HostConfig, Mount, MountTmpfsOptions, MountType, ResourcesUlimits,
};
use needy_config::profile::{ContainerExecutor as ContainerConfig, Device, Gpus, MemorySwap, Mount as ProfileMount, Ulimit};
use needy_config::value::ByteSize;

use super::Options;
use crate::driver::{Error, Spec, host_path, sanitize};

/// Where the instance's socket directory is mounted in the container.
const SOCKET_DIR: &str = "/run/needy";

/// A container the driver is about to create.
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
  pub(super) fn socket(&self) -> PathBuf {
    self.socket_dir.join(SOCKET)
  }
}

/// The executor's socket in its socket directory.
const SOCKET: &str = "executor.sock";

/// The host directory with the socket of the container `name`: twelve hex
/// digits derived from the name, so a long key cannot make the socket's
/// path too long.
pub(super) fn socket_dir(options: &Options, name: &str) -> PathBuf {
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
/// fields first, then the profile's typed `container:` fields under the
/// operator's policy.
pub fn container(spec: &Spec, options: &Options) -> Result<Container, Error> {
  let docker = spec.config.container.clone().unwrap_or_default();
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
    labels: Some(crate::engine::labels(&spec.run, "executor")),
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

/// Sets a profile's typed `container:` fields on the container's body. The
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
fn apply_run_options(docker: &ContainerConfig, base: &Path, allow: &[String], container: &mut Container) -> Result<(), Error> {
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
