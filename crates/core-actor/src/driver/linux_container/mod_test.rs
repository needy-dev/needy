use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use needy_config::Source;
use needy_config::profile::User;
use needy_core::recipe::{CompiledRecipe, HostInfo};
use needy_core_executor::engine::{Error as EngineError, Unreachable};

use super::*;

/// An engine that records what it is asked and whose containers exit
/// with `code` once waited for, after printing `log`.
#[derive(Default)]
struct Fake {
  calls: Mutex<Vec<String>>,
  bodies: Mutex<Vec<ContainerCreateBody>>,
  runtimes: Vec<(&'static str, &'static [&'static str])>,
  /// The images it has, by name.
  images: Vec<(String, engine::Image)>,
  code: i64,
  log: &'static str,
}

impl Fake {
  fn calls(&self) -> Vec<String> {
    self.calls.lock().unwrap().clone()
  }

  fn record(&self, call: String) {
    self.calls.lock().unwrap().push(call);
  }
}

#[async_trait::async_trait]
impl Engine for Fake {
  async fn runtimes(&self) -> Result<BTreeMap<String, Vec<String>>, EngineError> {
    Ok(self.runtimes.iter().map(|(name, args)| (name.to_string(), args.iter().map(|a| a.to_string()).collect())).collect())
  }

  async fn inspect_image(&self, image: &str) -> Result<Option<engine::Image>, EngineError> {
    Ok(self.images.iter().find(|(name, _)| name == image).map(|(_, found)| found.clone()))
  }

  async fn pull(&self, image: &str) -> Result<(), EngineError> {
    self.record(format!("pull {image}"));
    Ok(())
  }

  async fn create(&self, name: &str, body: ContainerCreateBody) -> Result<(), EngineError> {
    self.record(format!("create {name}"));
    self.bodies.lock().unwrap().push(body);
    Ok(())
  }

  async fn start(&self, name: &str) -> Result<u32, EngineError> {
    self.record(format!("start {name}"));
    Ok(4242)
  }

  fn logs(&self, _name: &str) -> BoxStream<'static, Result<bytes::Bytes, EngineError>> {
    futures_util::stream::iter([Ok(bytes::Bytes::from_static(self.log.as_bytes()))]).boxed()
  }

  async fn wait(&self, name: &str) -> Result<i64, EngineError> {
    self.record(format!("wait {name}"));
    Ok(self.code)
  }

  async fn stop(&self, name: &str, grace: Duration) -> Result<(), EngineError> {
    self.record(format!("stop {name} {}s", grace.as_secs()));
    Ok(())
  }

  async fn remove(&self, name: &str) -> Result<(), EngineError> {
    self.record(format!("remove {name}"));
    Ok(())
  }

  /// The image has an account file only at `/etc/passwd`: `agent` at 1000.
  async fn download(&self, name: &str, path: &str) -> Result<Option<Vec<u8>>, EngineError> {
    self.record(format!("download {name} {path}"));
    if path != "/etc/passwd" {
      return Ok(None);
    }
    let text = PASSWD.as_bytes();
    let mut archive = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(text.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    archive.append_data(&mut header, "passwd", text).unwrap();
    Ok(Some(archive.into_inner().unwrap()))
  }
}

/// The image's `/etc/passwd` in [`Fake`].
const PASSWD: &str = "root:x:0:0:root:/root:/bin/bash\nagent:x:1000:1000::/home/agent:/bin/bash\n";

/// The guest needy a test's actor gets.
const GUEST_NEEDY: &str = "/home/user/.local/bin/needy";

/// `needy-actor:latest` for this host's actors, with the base ABI label
/// `abi`, or none.
fn image(abi: Option<&str>) -> (String, engine::Image) {
  image_of("needy-actor:latest", abi, container::oci_arch(crate::image::guest::arch()))
}

/// `name` for `linux/architecture`, with the base ABI label `abi`, or none.
fn image_of(name: &str, abi: Option<&str>, architecture: &str) -> (String, engine::Image) {
  let labels = abi.map(|abi| HashMap::from([(base::LABEL.to_string(), abi.to_string())])).unwrap_or_default();
  let image = engine::Image {
    id: "sha256:0123".into(),
    os: "linux".into(),
    architecture: architecture.into(),
    labels,
  };
  (name.into(), image)
}

#[tokio::test]
async fn a_custom_image_without_a_base_abi_names_the_base_to_rebuild_from() {
  let fake = Arc::new(Fake {
    runtimes: vec![("runc", &[])],
    images: vec![image_of(
      "needy-custom-jq",
      None,
      container::oci_arch(crate::image::guest::arch()),
    )],
    ..Default::default()
  });
  let container = LinuxContainer::with_engine(OciRuntime::Runc, "needy-custom-jq".into(), fake);

  let error = container.check_host().await.unwrap_err().to_string();

  assert!(error.starts_with("the actor image needy-custom-jq records no base ABI"), "{error}");
  assert!(
    error.contains("rebuild it FROM a base image of base ABI 1: `needy-actor:latest` after `needy build --linux-container`"),
    "{error}"
  );
}

#[tokio::test]
async fn an_image_of_another_architecture_is_refused_before_the_actor_starts() {
  let other = if crate::image::guest::arch() == "x86_64" {
    "arm64"
  } else {
    "amd64"
  };
  let fake = Arc::new(Fake {
    runtimes: vec![("runc", &[])],
    images: vec![image_of("needy-custom-jq", Some("1"), other)],
    ..Default::default()
  });
  let container = LinuxContainer::with_engine(OciRuntime::Runc, "needy-custom-jq".into(), fake);

  let error = container.check_host().await.unwrap_err().to_string();

  assert!(error.starts_with(&format!("the actor image needy-custom-jq is linux/{other}, and actors here run linux/")), "{error}");
}

fn runtime(fake: Fake, oci: OciRuntime) -> (LinuxContainer, Arc<Fake>) {
  let fake = Arc::new(fake);
  (LinuxContainer::with_engine(oci, "needy-actor:latest".into(), fake.clone()), fake)
}

fn gce() -> CompiledRecipe {
  let recipe = needy_config::recipe::load(&Source::builtin("gce-ubuntu-22.04")).unwrap();
  needy_core::recipe::compile(&recipe, &HostInfo::default(), &needy_core::recipe::PerRun::at(7, 0))
}

fn start<'a>(dir: &'a Path, user: &'a User, recipe: Option<&'a CompiledRecipe>) -> Start<'a> {
  Start {
    run: "0123456789ab",
    dir,
    recipe,
    user,
    mounts: &[],
    guest: Path::new(GUEST_NEEDY),
  }
}

#[test]
fn the_channels_are_sockets_in_the_directory_bind_mounted_into_the_container() {
  let (container, _) = runtime(Fake::default(), OciRuntime::Runc);
  let dir = Path::new("/home/user/.local/state/needy/runs/0123456789ab");

  let channels = container.channels(dir);

  assert_eq!(channels.guest, Endpoint::Unix(dir.join("channels/guest.sock")));
  assert_eq!(channels.gateway, Endpoint::Unix(dir.join("channels/gateway.sock")));
  assert_eq!(channels.terminal, Terminal::Unix(dir.join("channels/process.sock")));
  assert_eq!(FACETS.transport, Transport::Socket, "the driving program's proxy runs as the profile's user");
  let user = User::default();
  let launch = container.launch(&start(dir, &user, None), Vec::new());
  let binds = launch.body.host_config.unwrap().binds.unwrap();
  assert_eq!(binds[0], format!("{}:/.needy/channels", dir.join("channels").display()));
}

#[test]
fn the_guest_needy_is_bind_mounted_read_only_over_the_base_images_placeholder() {
  let (container, _) = runtime(Fake::default(), OciRuntime::Runc);
  let user = User::default();

  let launch = container.launch(&start(Path::new("/r"), &user, None), vec!["/r/cpuinfo:/proc/cpuinfo:ro".into()]);

  let binds = launch.body.host_config.unwrap().binds.unwrap();
  assert_eq!(
    binds,
    [
      "/r/channels:/.needy/channels",
      "/home/user/.local/bin/needy:/.needy/needy:ro",
      "/r/cpuinfo:/proc/cpuinfo:ro"
    ]
  );
  // The base image's boot stub starts it.
  assert_eq!(launch.body.entrypoint, Some(vec!["/.needy/init".to_string()]));
}

#[tokio::test]
async fn an_image_of_another_base_abi_is_refused_before_the_actor_starts() {
  let (container, _) = runtime(
    Fake {
      runtimes: vec![("runc", &[])],
      images: vec![image(Some("2"))],
      ..Default::default()
    },
    OciRuntime::Runc,
  );

  let error = container.check_host().await.unwrap_err().to_string();

  assert_eq!(
    error,
    "the actor image needy-actor:latest has base ABI 2, and this needy speaks base ABI 1; rebuild it with `needy build \
     --linux-container` from this needy's version, or use a needy that speaks base ABI 2"
  );
}

#[tokio::test]
async fn an_image_built_with_its_own_needy_inside_is_refused() {
  let (container, _) = runtime(
    Fake {
      runtimes: vec![("runc", &[])],
      images: vec![image(None)],
      ..Default::default()
    },
    OciRuntime::Runc,
  );

  let error = container.check_host().await.unwrap_err().to_string();

  assert!(error.starts_with("the actor image needy-actor:latest records no base ABI"), "{error}");
}

#[test]
fn the_container_has_no_network_a_read_only_root_and_the_oci_runtime_of_the_profile() {
  let (container, _) = runtime(Fake::default(), OciRuntime::Runsc);
  let user = User::default();

  let launch = container.launch(&start(Path::new("/r"), &user, None), Vec::new());

  let host = launch.body.host_config.unwrap();
  assert_eq!(host.runtime.as_deref(), Some("runsc"));
  assert_eq!(host.network_mode.as_deref(), Some("none"));
  assert_eq!(host.readonly_rootfs, Some(true));
  assert_eq!(launch.body.entrypoint, Some(vec!["/.needy/init".to_string()]));
  assert_eq!(launch.body.user.as_deref(), Some("0:0"));
  assert_eq!(launch.name, "needy-actor-0123456789ab");
}

#[test]
fn the_home_directory_is_a_tmpfs_the_user_owns_where_programs_may_run() {
  let (container, _) = runtime(Fake::default(), OciRuntime::Runc);
  let user = User {
    name: "dev".into(),
    uid: 1001,
    gid: 1002,
  };

  let launch = container.launch(&start(Path::new("/r"), &user, None), Vec::new());

  let tmpfs = launch.body.host_config.unwrap().tmpfs.unwrap();
  assert_eq!(tmpfs["/home/dev"], "rw,exec,nosuid,nodev,mode=755,uid=1001,gid=1002");
  assert!(tmpfs["/tmp"].contains("exec") && !tmpfs["/tmp"].contains("noexec"), "{}", tmpfs["/tmp"]);
}

#[test]
fn a_recipe_sets_the_hostname_and_the_memory_limit() {
  let (container, _) = runtime(Fake::default(), OciRuntime::Runc);
  let recipe = gce();
  let user = User::default();

  let launch = container.launch(&start(Path::new("/r"), &user, Some(&recipe)), Vec::new());

  assert_eq!(launch.body.hostname, recipe.hostname);
  assert_eq!((launch.vcpus, launch.mem_mib), (2, 8192));
  assert_eq!(launch.body.host_config.unwrap().memory, Some(8192 << 20));
}

#[tokio::test]
async fn a_host_without_the_oci_runtime_names_how_to_install_gvisor() {
  let (container, _) = runtime(
    Fake {
      runtimes: vec![("runc", &[])],
      images: vec![image(Some("1"))],
      ..Default::default()
    },
    OciRuntime::Runsc,
  );

  let error = container.check_host().await.unwrap_err().to_string();

  assert!(error.contains("Docker has no runsc runtime (it has runc)"), "{error}");
  assert!(error.contains("https://gvisor.dev/docs/user_guide/install/"), "{error}");
}

#[tokio::test]
async fn a_host_without_the_image_names_the_build_that_makes_it() {
  let (container, _) = runtime(
    Fake {
      runtimes: vec![("runc", &[])],
      ..Default::default()
    },
    OciRuntime::Runc,
  );

  let error = container.check_host().await.unwrap_err().to_string();

  assert!(error.contains("needy build --linux-container"), "{error}");
}

#[tokio::test]
async fn a_started_actor_logs_its_output_and_is_removed_when_it_exits() {
  let dir = tempfile::tempdir().unwrap();
  let (container, fake) = runtime(
    Fake {
      code: 3,
      log: "init: booted\n",
      ..Default::default()
    },
    OciRuntime::Runc,
  );
  let user = User::default();

  let mut actor = container.start(&start(dir.path(), &user, None)).await.unwrap();
  let status = actor.wait().await.unwrap();

  assert_eq!(status.code(), Some(3));
  assert_eq!(actor.pid(), 4242);
  assert_eq!(std::fs::read_to_string(dir.path().join("console.log")).unwrap(), "init: booted\n");
  let name = "needy-actor-0123456789ab";
  let scratch = format!("{name}-account");
  assert_eq!(
    fake.calls(),
    [
      format!("remove {scratch}"),
      format!("create {scratch}"),
      format!("download {scratch} /etc/passwd"),
      format!("download {scratch} /etc/group"),
      format!("download {scratch} /etc/shadow"),
      format!("remove {scratch}"),
      format!("remove {name}"),
      format!("create {name}"),
      format!("start {name}"),
      format!("wait {name}"),
      format!("remove {name}")
    ]
  );
}

#[tokio::test]
async fn stopping_an_actor_stops_its_container_within_the_grace_and_removes_it() {
  let dir = tempfile::tempdir().unwrap();
  let (container, fake) = runtime(
    Fake {
      code: 137,
      ..Default::default()
    },
    OciRuntime::Runc,
  );
  let user = User::default();

  let mut actor = container.start(&start(dir.path(), &user, None)).await.unwrap();
  let status = actor.stop(Duration::from_secs(3)).await.unwrap();

  assert_eq!(status.code(), Some(137));
  let calls = fake.calls();
  assert_eq!(
    calls[calls.len() - 3..],
    [
      "stop needy-actor-0123456789ab 3s",
      "wait needy-actor-0123456789ab",
      "remove needy-actor-0123456789ab"
    ]
  );
}

// ROOT CAUSE:
//
// If the engine's socket could not be found, `profile check` reported nothing
// about the container actor, because the runtime failed to construct and
// the check skips a runtime it cannot construct as a profile error.
//
// Before the fix, a host without an engine passed the actor's host check.
// The fix makes an unreachable engine fail each use, `check_host` first.
#[tokio::test]
async fn an_engine_that_cannot_be_reached_fails_the_host_check_with_the_reason() {
  let container = LinuxContainer::with_engine(OciRuntime::Runc, "needy-actor:latest".into(), Arc::new(Unreachable("socket missing".into())));

  let error = container.check_host().await.unwrap_err().to_string();

  assert_eq!(error, "no Docker-compatible engine answers: socket missing");
}

// ROOT CAUSE:
//
// If Docker ran gVisor with its defaults, the actor's init could not reach
// the daemon, because gVisor keeps containers from host Unix sockets and the
// actor's channels are bind-mounted host sockets.
//
// Before the fix, a `runsc` actor started and failed after 5 s with `cannot
// reach the daemon at unix:/.needy/channels/guest.sock`.
// The fix refuses the run up front, naming the registration that works.
#[tokio::test]
async fn gvisor_without_host_sockets_is_refused_with_the_registration_that_works() {
  let (container, _) = runtime(
    Fake {
      runtimes: vec![("runc", &[]), ("runsc", &[])],
      images: vec![image(Some("1"))],
      ..Default::default()
    },
    OciRuntime::Runsc,
  );

  let error = container.check_host().await.unwrap_err().to_string();

  assert!(error.contains("runsc install -- --host-uds=all"), "{error}");
}

#[tokio::test]
async fn gvisor_with_host_sockets_and_the_image_passes_the_host_check() {
  let (container, _) = runtime(
    Fake {
      runtimes: vec![("runsc", &["--host-uds=all"])],
      images: vec![image(Some("1"))],
      ..Default::default()
    },
    OciRuntime::Runsc,
  );

  container.check_host().await.unwrap();
}

// ROOT CAUSE:
//
// If a profile renamed its user (`bob`, uid 1001), the actor image's
// `/etc/passwd` still had only `agent`, so the driving program got
// `HOME=/home/bob` while `whoami` said `agent` or nothing.
//
// Before the fix, a container actor kept the image's account files.
// The fix binds account files rewritten for the profile's user, read-only.
#[tokio::test]
async fn a_renamed_user_gets_its_passwd_entry_bound_into_the_container() {
  let dir = tempfile::tempdir().unwrap();
  let (container, fake) = runtime(Fake::default(), OciRuntime::Runc);
  let user = User {
    name: "bob".into(),
    uid: 1001,
    gid: 1001,
  };

  container.start(&start(dir.path(), &user, None)).await.unwrap();

  let passwd = dir.path().join("account").join("passwd");
  assert_eq!(std::fs::read_to_string(&passwd).unwrap(), format!("{PASSWD}bob:x:1001:1001::/home/bob:/bin/bash\n"));
  let bodies = fake.bodies.lock().unwrap();
  let binds = bodies.last().unwrap().host_config.as_ref().unwrap().binds.clone().unwrap();
  assert!(binds.contains(&format!("{}:/etc/passwd:ro", passwd.display())), "{binds:?}");
  assert!(!binds.iter().any(|bind| bind.ends_with(":/etc/group:ro")), "{binds:?}");
}
