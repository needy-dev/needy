// The tests that use these helpers are `cfg(unix)`; on Windows they are
// left unused.
#![cfg_attr(not(unix), allow(dead_code))]

use std::path::{Path, PathBuf};

use needy_config::Source;
use needy_config::profile::{ContainerExecutor as ContainerConfig, Executor, User};

use crate::engine::models::{ContainerCreateBody, MountType};

use super::*;

/// The `container:` options of a profile's executor, written as YAML.
fn docker(yaml: &str) -> ContainerConfig {
  let text = format!("actor: {{ driver: linux-firecracker }}\nexecutors: {{ build: {{ driver: linux-container, container: {yaml} }} }}");
  let profile = needy_config::profile::load(&Source::text(text, "/home/user/profile")).unwrap();
  profile.executors["build"].container.clone().unwrap()
}

fn spec(docker: ContainerConfig) -> Spec {
  Spec {
    name: "build".into(),
    key: "build@s1".into(),
    run: "s1".into(),
    config: Executor {
      container: Some(docker),
      ..Default::default()
    },
    base: PathBuf::from("/home/user/profile"),
    user: User::default(),
    hostname: Some("instance-1".into()),
  }
}

fn options() -> Options {
  Options {
    image: "needy-executor:latest".into(),
    state_dir: PathBuf::from("/home/user/state"),
    allow: Vec::new(),
  }
}

/// The socket directory's name for container `name`.
fn socket_id(name: &str) -> String {
  uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, name.as_bytes()).simple().to_string()[..12].to_string()
}

/// The container for `container:` options, as YAML.
fn body(yaml: &str, options: &Options) -> ContainerCreateBody {
  container(&spec(docker(yaml)), options).unwrap().body
}

fn refusal(yaml: &str, options: &Options) -> String {
  match container(&spec(docker(yaml)), options) {
    Ok(container) => panic!("{yaml} was accepted: {container:?}"),
    Err(e) => e.to_string(),
  }
}

/// Options of an operator who allows `names`.
fn operator(names: &[&str]) -> Options {
  Options {
    allow: names.iter().map(|n| n.to_string()).collect(),
    ..options()
  }
}

#[test]
fn the_daemon_sets_name_user_hostname_label_mounts_and_command() {
  let container = container(&spec(ContainerConfig::default()), &options()).unwrap();

  assert_eq!(container.name, "needy-build-s1");
  assert_eq!(container.socket_dir, Path::new("/home/user/state/executors").join(socket_id("needy-build-s1")));
  assert_eq!(container.workspace_dir, Path::new("/home/user/state/workspaces/build-s1"));
  assert_eq!(container.workspace, "/home/agent/workspace");
  let body = &container.body;
  assert_eq!(body.image.as_deref(), Some("needy-executor:latest"));
  assert_eq!(body.user.as_deref(), Some("1000:1000"));
  // The recipe's hostname, so the actor and its executors agree on it.
  assert_eq!(body.hostname.as_deref(), Some("instance-1"));
  assert_eq!(body.labels.as_ref().unwrap()["needy.run"], "s1");
  assert_eq!(body.labels.as_ref().unwrap()["needy.role"], "executor");
  assert_eq!(
    body.cmd.as_deref().unwrap(),
    [
      "needy",
      "daemon",
      "--mode",
      "executor",
      "--listen",
      "unix:/run/needy/executor.sock",
      "--name",
      "build"
    ]
  );
  let host = body.host_config.as_ref().unwrap();
  assert_eq!(host.auto_remove, Some(true));
  assert_eq!(
    host.binds.as_deref().unwrap(),
    [
      "/home/user/state/workspaces/build-s1:/home/agent/workspace",
      &format!("/home/user/state/executors/{}:/run/needy", socket_id("needy-build-s1"))
    ]
  );
}

#[test]
fn a_profile_image_and_workspace_replace_the_defaults() {
  let mut spec = spec(ContainerConfig::default());
  spec.config.workspace = Some("ws".into());
  spec.config.container.as_mut().unwrap().image = Some("example.com/toolchain:1".into());
  spec.hostname = None;

  let container = container(&spec, &options()).unwrap();

  assert_eq!(container.workspace_dir, Path::new("/home/user/profile/ws"));
  assert_eq!(container.body.image.as_deref(), Some("example.com/toolchain:1"));
  assert_eq!(container.body.hostname, None);
}

#[test]
fn resource_fields_become_limits() {
  let body = body(
    "{ cpus: 1.5, cpu_shares: 512, cpuset_cpus: 0-3, memory: 8GiB, memory_swap: unlimited, shm_size: 64MiB, pids_limit: 512, \
     ulimits: { nofile: 1024, nproc: { soft: 64, hard: 128 } } }",
    &options(),
  );

  let host = body.host_config.unwrap();
  assert_eq!(host.nano_cpus, Some(1_500_000_000));
  assert_eq!(host.cpu_shares, Some(512));
  assert_eq!(host.cpuset_cpus.as_deref(), Some("0-3"));
  assert_eq!(host.memory, Some(8 << 30));
  assert_eq!(host.memory_swap, Some(-1));
  assert_eq!(host.shm_size, Some(64 << 20));
  assert_eq!(host.pids_limit, Some(512));
  let ulimits: Vec<_> = host.ulimits.unwrap().into_iter().map(|u| (u.name.unwrap(), u.soft.unwrap(), u.hard.unwrap())).collect();
  assert_eq!(
    ulimits,
    [
      ("nofile".to_string(), 1024, 1024),
      ("nproc".to_string(), 64, 128)
    ]
  );
}

#[test]
fn absent_fields_leave_dockers_defaults() {
  let body = body("{}", &options());

  let host = body.host_config.unwrap();
  assert_eq!((host.nano_cpus, host.memory, host.memory_swap, host.init, host.readonly_rootfs), (None, None, None, None, None));
  assert_eq!((host.mounts, host.device_requests, host.extra_hosts, host.dns, host.sysctls), (None, None, None, None, None));
  assert_eq!((body.env, body.working_dir), (None, None));
}

#[test]
fn process_and_name_fields_become_the_container_and_host_config() {
  let body = body(
    "{ env: { RUST_LOG: debug, A: \"1\" }, workdir: /src, read_only: true, init: true, add_host: { db.example.com: 192.0.2.10 }, \
     dns: [192.0.2.53, \"2001:db8::53\"], sysctls: { net.ipv4.ip_forward: \"1\" }, cap_drop: [NET_RAW], pid: \"container:other\" }",
    &options(),
  );

  assert_eq!(body.env.as_deref().unwrap(), ["A=1", "RUST_LOG=debug"]);
  assert_eq!(body.working_dir.as_deref(), Some("/src"));
  let host = body.host_config.unwrap();
  assert_eq!((host.readonly_rootfs, host.init), (Some(true), Some(true)));
  assert_eq!(host.extra_hosts.unwrap(), ["db.example.com:192.0.2.10"]);
  assert_eq!(host.dns.unwrap(), ["192.0.2.53", "2001:db8::53"]);
  assert_eq!(host.sysctls.unwrap()["net.ipv4.ip_forward"], "1");
  assert_eq!(host.cap_drop.unwrap(), ["NET_RAW"]);
  assert_eq!(host.pid_mode.as_deref(), Some("container:other"));
}

/// A GPU request as (count, device IDs, capabilities).
type GpuRequest = (Option<i64>, Option<Vec<String>>, Option<Vec<Vec<String>>>);

/// The GPU request `gpus` becomes.
fn gpu_request(gpus: &str) -> GpuRequest {
  let mut requests = body(&format!("{{ gpus: {gpus} }}"), &operator(&["gpus"])).host_config.unwrap().device_requests.unwrap();
  assert_eq!(requests.len(), 1);
  let request = requests.remove(0);
  assert_eq!(request.driver, None, "the runtime's default driver");
  (request.count, request.device_ids, request.capabilities)
}

#[test]
fn gpus_become_a_device_request_as_docker_run_makes_it() {
  let gpu = Some(vec![vec!["gpu".to_string()]]);
  assert_eq!(gpu_request("all"), (Some(-1), None, gpu.clone()));
  assert_eq!(gpu_request("2"), (Some(2), None, gpu.clone()));
  assert_eq!(gpu_request("[\"0\", GPU-example]"), (None, Some(vec!["0".into(), "GPU-example".into()]), gpu));
}

#[test]
fn profile_mounts_follow_the_daemon_mounts() {
  let mut spec = spec(docker(
    "{ mounts: [{ type: bind, source: ws/cache, target: /cache, read_only: true }, { type: volume, source: cargo, target: /cargo }, \
     { type: volume, target: /scratch }, { type: tmpfs, target: /tmp, size: 64MiB }] }",
  ));
  spec.config.workspace = Some("ws".into());

  let host = container(&spec, &options()).unwrap().body.host_config.unwrap();

  assert_eq!(host.binds.unwrap().len(), 2, "the workspace and the socket");
  let mounts: Vec<_> = host
    .mounts
    .unwrap()
    .into_iter()
    .map(|m| (m.typ.unwrap(), m.source, m.target.unwrap(), m.read_only, m.tmpfs_options.and_then(|t| t.size_bytes)))
    .collect();
  assert_eq!(
    mounts,
    [
      (MountType::BIND, Some("/home/user/profile/ws/cache".into()), "/cache".into(), Some(true), None),
      (MountType::VOLUME, Some("cargo".into()), "/cargo".into(), Some(false), None),
      (MountType::VOLUME, None, "/scratch".into(), Some(false), None),
      (MountType::TMPFS, None, "/tmp".into(), None, Some(64 << 20)),
    ]
  );
}

#[test]
fn mounts_over_the_daemons_or_at_relative_targets_are_refused() {
  assert_eq!(
    refusal("{ mounts: [{ type: volume, target: /home/agent/workspace/node_modules }] }", &options()),
    "the mount at /home/agent/workspace/node_modules is over the workspace, which the daemon mounts"
  );
  assert_eq!(
    refusal("{ mounts: [{ type: tmpfs, target: /run/needy }] }", &options()),
    "the mount at /run/needy is over the executor's socket directory, which the daemon mounts"
  );
  assert_eq!(refusal("{ mounts: [{ type: tmpfs, target: tmp }] }", &options()), "mounts: tmp is not an absolute path");
  assert_eq!(refusal("{ workdir: src }", &options()), "workdir: src is not an absolute path");
}

#[test]
fn privileged_fields_need_the_operator() {
  assert_eq!(refusal("{ privileged: true }", &options()), "privileged is not allowed by the operator");
  assert_eq!(refusal("{ cap_add: [SYS_ADMIN] }", &options()), "cap_add SYS_ADMIN is not allowed by the operator");
  assert_eq!(refusal("{ devices: [{ host: /dev/fuse }] }", &options()), "devices /dev/fuse is not allowed by the operator");
  assert_eq!(refusal("{ gpus: all }", &options()), "gpus all is not allowed by the operator");
  assert_eq!(refusal("{ gpus: [\"0\"] }", &options()), "gpus 0 is not allowed by the operator");
  assert_eq!(refusal("{ pid: host }", &options()), "pid host is not allowed by the operator");
  assert_eq!(refusal("{ userns: host }", &options()), "userns host is not allowed by the operator");
  assert_eq!(
    refusal("{ security_opt: [seccomp=unconfined] }", &options()),
    "security_opt seccomp=unconfined is not allowed by the operator"
  );
  assert!(container(&spec(docker("{ security_opt: [no-new-privileges] }")), &options()).is_ok());

  let host =
    body("{ privileged: true, cap_add: [SYS_ADMIN], pid: host }", &operator(&["privileged", "cap_add", "pid"])).host_config.unwrap();
  assert_eq!((host.privileged, host.pid_mode.as_deref()), (Some(true), Some("host")));
  assert_eq!(host.cap_add.unwrap(), ["SYS_ADMIN"]);
}

#[test]
fn a_bind_mount_outside_the_workspace_needs_the_operator() {
  for source in [
    "/",
    "/etc",
    "../elsewhere",
    "/home/user/state/workspaces/build-s1/../other",
  ] {
    let yaml = format!("{{ mounts: [{{ type: bind, source: \"{source}\", target: /host }}] }}");
    let message = refusal(&yaml, &options());
    assert!(message.ends_with("is outside the executor's workspace and is not allowed by the operator"), "{source}: {message}");
  }
  // The default workspace, under the daemon's state directory.
  assert!(
    container(&spec(docker("{ mounts: [{ type: bind, source: /home/user/state/workspaces/build-s1/c, target: /c }] }")), &options()).is_ok()
  );

  let host = body("{ mounts: [{ type: bind, source: /, target: /host }] }", &operator(&["host_mounts"])).host_config.unwrap();
  assert_eq!(host.mounts.unwrap()[0].source.as_deref(), Some("/"));
}

#[test]
fn a_device_defaults_to_the_hosts_path_and_every_permission() {
  let devices = |yaml: &str| {
    body(yaml, &operator(&["devices"]))
      .host_config
      .unwrap()
      .devices
      .unwrap()
      .into_iter()
      .map(|d| (d.path_on_host.unwrap(), d.path_in_container.unwrap(), d.cgroup_permissions.unwrap()))
      .collect::<Vec<_>>()
  };
  let own = |h: &str, c: &str, p: &str| (h.to_string(), c.to_string(), p.to_string());
  assert_eq!(
    devices("{ devices: [{ host: /dev/fuse }, { host: /dev/kvm, container: /dev/vm, permissions: rw }] }"),
    [
      own("/dev/fuse", "/dev/fuse", "rwm"),
      own("/dev/kvm", "/dev/vm", "rw")
    ]
  );
  assert_eq!(
    refusal("{ devices: [{ host: /dev/kvm, permissions: rx }] }", &operator(&["devices"])),
    "devices /dev/kvm: bad permissions rx: use some of r, w and m"
  );
  assert_eq!(refusal("{ devices: [{ host: /dev/kvm, container: vm }] }", &operator(&["devices"])), "devices: vm is not an absolute path");
}

/// Creates an executor container with typed run options from the executor
/// image, runs a command in it and destroys it.
/// Skips without Docker or without the image (`needy build --executor`), and
/// off Linux, where the host cannot reach a socket inside a container.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn create_and_destroy_an_executor_container() {
  use needy_api_proto::v1::{ExecRequest, ExecStart, exec_request, exec_response};

  let state = tempfile::tempdir().unwrap();
  // NOTICE: `NEEDY_TEST_EXECUTOR_IMAGE` names another image, such as one
  // built for a test matrix, so the test need not touch the operator's
  // `needy-executor:latest`.
  let options = Options {
    state_dir: state.path().to_path_buf(),
    image: std::env::var("NEEDY_TEST_EXECUTOR_IMAGE").unwrap_or_else(|_| options().image),
    ..options()
  };
  let driver = LinuxContainer::new(options.clone()).unwrap();
  // The test asks the engine itself, not through the driver, whether the
  // container is gone.
  let engine = bollard::Docker::connect_with_defaults().unwrap();
  if engine.ping().await.is_err() || engine.inspect_image(&options.image).await.is_err() {
    eprintln!("skipped: no Docker or no {} image", options.image);
    return;
  }
  let mut spec = spec(docker("{ memory: 256MiB, cpus: 0.5, env: { NEEDY_TEST: typed } }"));
  spec.key = format!("test-{}", uuid::Uuid::new_v4().simple());
  // The executor runs as the owner of the socket directory, which is the
  // test's user.
  use std::os::unix::fs::MetadataExt;
  let owner = state.path().metadata().unwrap();
  spec.user = User {
    name: "agent".into(),
    uid: owner.uid(),
    gid: owner.gid(),
  };

  let handle = driver.create(&spec).await.unwrap();

  let (tx, rx) = tokio::sync::mpsc::channel(1);
  tx.send(ExecRequest {
    payload: Some(exec_request::Payload::Start(ExecStart {
      argv: [
        "sh",
        "-c",
        "hostname; cat /sys/fs/cgroup/memory.max /sys/fs/cgroup/cpu.max; printenv NEEDY_TEST",
      ]
      .map(String::from)
      .to_vec(),
      no_stdin: true,
      ..Default::default()
    })),
  })
  .await
  .unwrap();
  let mut responses = handle.executor.exec(rx).await.unwrap();
  let mut stdout = Vec::new();
  while let Some(message) = tokio_stream::StreamExt::next(&mut responses).await.transpose().unwrap() {
    match message.payload {
      Some(exec_response::Payload::Stdout(bytes)) => stdout.extend(bytes),
      Some(exec_response::Payload::Exited(_)) => break,
      _ => {}
    }
  }
  // The recipe's hostname, then the typed fields as the container's cgroup
  // (v2) and environment see them: 256 MiB, half of a 100 ms period.
  assert_eq!(String::from_utf8_lossy(&stdout).trim(), "instance-1\n268435456\n50000 100000\ntyped");
  assert_eq!(handle.driver, "linux-container");

  driver.destroy(&handle).await.unwrap();
  let gone = engine.inspect_container(&handle.id, None).await;
  assert!(
    matches!(
      gone,
      Err(bollard::errors::Error::DockerResponseServerError {
        status_code: 404,
        ..
      })
    ) || gone.is_ok_and(|c| c.state.and_then(|s| s.running) == Some(false)),
    "the container is removed"
  );
}

// ROOT CAUSE:
//
// If a Docker executor's key was long (a group or profile in its template)
// or the state directory was deep, the instance's socket path went over the
// Unix socket limit (`sun_path`, 108 bytes on Linux), because the socket
// directory was named after the whole key.
//
// Before the fix, `executors/<key>/executor.sock` under a project's scratch
// state directory left about 13 bytes to spare, and a longer path made the
// create time out waiting for a socket the container could not bind.
// The fix keeps the directory's name to twelve characters derived from the
// key, and refuses a path that still does not fit, naming it.
#[test]
fn the_socket_directory_is_short_whatever_the_key() {
  let mut long = spec(ContainerConfig::default());
  long.key = format!("build@{}", "group-".repeat(20));

  let container = container(&long, &options()).unwrap();

  let name = container.socket_dir.strip_prefix("/home/user/state/executors").unwrap();
  assert_eq!(name.as_os_str().len(), 12, "{}", container.socket_dir.display());
  assert_eq!(container.socket_dir, super::container(&long, &options()).unwrap().socket_dir, "one key, one directory");
  assert_ne!(container.socket_dir, super::container(&spec(ContainerConfig::default()), &options()).unwrap().socket_dir);
}

#[cfg(unix)]
#[test]
fn a_socket_path_over_sun_path_is_refused_with_the_path() {
  let options = Options {
    state_dir: PathBuf::from("/home/user").join("s".repeat(100)),
    ..options()
  };

  let message = refusal("{}", &options);

  assert!(message.contains(&options.state_dir.display().to_string()), "{message}");
  assert!(message.contains("too long"), "{message}");
}

fn user(uid: u32) -> User {
  User {
    name: "agent".into(),
    uid,
    gid: uid,
  }
}

#[cfg(unix)]
#[test]
fn a_directory_for_a_container_of_the_daemons_own_user_is_private() {
  use std::os::unix::fs::PermissionsExt;
  let dir = tempfile::tempdir().unwrap();
  let shared = dir.path().join("executors").join("abc");
  let me = nix::unistd::geteuid().as_raw();

  share_with_container(&shared, &user(me)).unwrap();

  assert_eq!(std::fs::metadata(&shared).unwrap().permissions().mode() & 0o777, 0o700);
}

// The 2026-11-10 run on additional cloud hosts: a daemon running as root made
// the socket directory `root 0755` and started the container as 1000:1000,
// whose executor could not create its socket; `run create` waited 10 s
// and failed with a transport error that did not mention permissions.
#[cfg(unix)]
#[test]
fn a_directory_for_a_container_of_another_user_is_theirs_or_the_error_says_why() {
  use std::os::unix::fs::{MetadataExt, PermissionsExt};
  let dir = tempfile::tempdir().unwrap();
  let shared = dir.path().join("executors").join("abc");
  let me = nix::unistd::geteuid().as_raw();
  let other = if me == 4242 { 4243 } else { 4242 };

  let shared_result = share_with_container(&shared, &user(other));

  if me == 0 {
    shared_result.unwrap();
    let metadata = std::fs::metadata(&shared).unwrap();
    assert_eq!((metadata.uid(), metadata.gid()), (other, other));
    assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
  } else {
    let message = anyhow_like(shared_result.unwrap_err());
    assert!(message.contains(&shared.display().to_string()), "{message}");
    assert!(message.contains(&format!("{other}:{other}")), "{message}");
    assert!(message.contains("cannot create its socket"), "{message}");
  }
}

/// An error with its causes, as a client prints them.
fn anyhow_like(error: Error) -> String {
  let mut text = error.to_string();
  let mut source = std::error::Error::source(&error);
  while let Some(cause) = source {
    text.push_str(&format!(": {cause}"));
    source = cause.source();
  }
  text
}

/// An engine without the executor's image until it is pulled, whose
/// containers do not start.
#[derive(Default)]
struct Fake {
  calls: std::sync::Mutex<Vec<String>>,
}

impl Fake {
  fn record(&self, call: String) {
    self.calls.lock().unwrap().push(call);
  }

  fn pulled(&self) -> bool {
    self.calls.lock().unwrap().iter().any(|call| call.starts_with("pull"))
  }
}

#[async_trait::async_trait]
impl Engine for Fake {
  async fn runtimes(&self) -> Result<std::collections::BTreeMap<String, Vec<String>>, engine::Error> {
    Ok(Default::default())
  }

  async fn inspect_image(&self, _image: &str) -> Result<Option<engine::Image>, engine::Error> {
    Ok(self.pulled().then(Default::default))
  }

  async fn pull(&self, image: &str) -> Result<(), engine::Error> {
    self.record(format!("pull {image}"));
    Ok(())
  }

  async fn create(&self, name: &str, _body: ContainerCreateBody) -> Result<(), engine::Error> {
    self.record(format!("create {name}"));
    if self.pulled() {
      Ok(())
    } else {
      Err(engine::Error::NotFound("No such image".into()))
    }
  }

  async fn start(&self, name: &str) -> Result<u32, engine::Error> {
    self.record(format!("start {name}"));
    Err(engine::Error::Refused("cannot start".into()))
  }

  fn logs(&self, _name: &str) -> futures_util::stream::BoxStream<'static, Result<bytes::Bytes, engine::Error>> {
    Box::pin(futures_util::stream::empty())
  }

  async fn wait(&self, _name: &str) -> Result<i64, engine::Error> {
    Ok(0)
  }

  async fn stop(&self, _name: &str, _grace: std::time::Duration) -> Result<(), engine::Error> {
    Ok(())
  }

  async fn remove(&self, name: &str) -> Result<(), engine::Error> {
    self.record(format!("remove {name}"));
    Ok(())
  }

  async fn download(&self, name: &str, path: &str) -> Result<Option<Vec<u8>>, engine::Error> {
    self.record(format!("download {name} {path}"));
    Ok(None)
  }
}

#[cfg(unix)]
#[tokio::test]
async fn a_missing_image_is_pulled_as_docker_run_does_and_a_failed_start_removes_the_container() {
  let state = tempfile::tempdir().unwrap();
  let options = Options {
    state_dir: state.path().to_path_buf(),
    ..options()
  };
  let fake = Arc::new(Fake::default());
  let driver = LinuxContainer::with_engine(options, fake.clone());
  let mut spec = spec(ContainerConfig::default());
  spec.user = user(nix::unistd::geteuid().as_raw());

  let error = driver.create(&spec).await.err().unwrap();

  assert_eq!(error.to_string(), "docker: cannot start");
  let name = "needy-build-s1";
  let scratch = format!("{name}-account");
  assert_eq!(
    *fake.calls.lock().unwrap(),
    [
      format!("remove {scratch}"),
      format!("create {scratch}"),
      "pull needy-executor:latest".to_string(),
      format!("create {scratch}"),
      format!("download {scratch} /etc/passwd"),
      format!("download {scratch} /etc/group"),
      format!("download {scratch} /etc/shadow"),
      format!("remove {scratch}"),
      format!("remove {name}"),
      format!("create {name}"),
      format!("start {name}"),
      format!("remove {name}"),
    ]
  );
}
