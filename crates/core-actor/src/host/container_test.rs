use std::sync::Mutex;

use needy_config::Source;
use needy_config::profile::User;
use needy_core::recipe::{CompiledRecipe, HostInfo};

use super::*;

/// A Docker daemon that records what it is asked and whose containers exit
/// with `code` once waited for, after printing `log`.
#[derive(Default)]
struct Fake {
  calls: Mutex<Vec<String>>,
  bodies: Mutex<Vec<ContainerCreateBody>>,
  runtimes: Vec<String>,
  images: Vec<String>,
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
  async fn runtimes(&self) -> Result<BTreeSet<String>, Error> {
    Ok(self.runtimes.iter().cloned().collect())
  }

  async fn has_image(&self, image: &str) -> Result<bool, Error> {
    Ok(self.images.iter().any(|i| i == image))
  }

  async fn create(&self, name: &str, body: ContainerCreateBody) -> Result<(), Error> {
    self.record(format!("create {name}"));
    self.bodies.lock().unwrap().push(body);
    Ok(())
  }

  async fn start(&self, name: &str) -> Result<u32, Error> {
    self.record(format!("start {name}"));
    Ok(4242)
  }

  fn logs(&self, _name: &str) -> BoxStream<'static, Result<bytes::Bytes, Error>> {
    futures_util::stream::iter([Ok(bytes::Bytes::from_static(self.log.as_bytes()))]).boxed()
  }

  async fn wait(&self, name: &str) -> Result<i64, Error> {
    self.record(format!("wait {name}"));
    Ok(self.code)
  }

  async fn stop(&self, name: &str, grace: Duration) -> Result<(), Error> {
    self.record(format!("stop {name} {}s", grace.as_secs()));
    Ok(())
  }

  async fn remove(&self, name: &str) -> Result<(), Error> {
    self.record(format!("remove {name}"));
    Ok(())
  }
}

fn runtime(fake: Fake, oci: OciRuntime) -> (Container, Arc<Fake>) {
  let fake = Arc::new(fake);
  (Container::with_engine(oci, "needy-actor:latest".into(), fake.clone()), fake)
}

fn gce() -> CompiledRecipe {
  let recipe = needy_config::recipe::load(&Source::builtin("gce-ubuntu-22.04")).unwrap();
  needy_core::recipe::compile(&recipe, &HostInfo::default(), 7)
}

fn start<'a>(dir: &'a Path, user: &'a User, recipe: Option<&'a CompiledRecipe>) -> Start<'a> {
  Start {
    run: "0123456789ab",
    dir,
    recipe,
    user,
    mounts: &[],
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
  assert!(channels.guest_for_every_user, "the driving program's proxy runs as the profile's user");
  let user = User::default();
  let launch = container.launch(&start(dir, &user, None), Vec::new());
  let binds = launch.body.host_config.unwrap().binds.unwrap();
  assert_eq!(
    binds,
    [format!(
      "{}:/.needy/channels",
      dir.join("channels").display()
    )]
  );
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
      runtimes: vec!["runc".into()],
      images: vec!["needy-actor:latest".into()],
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
      runtimes: vec!["runc".into()],
      ..Default::default()
    },
    OciRuntime::Runc,
  );

  let error = container.check_host().await.unwrap_err().to_string();

  assert!(error.contains("needy build --container"), "{error}");
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
  assert_eq!(
    fake.calls(),
    [
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
    calls[3..],
    [
      "stop needy-actor-0123456789ab 3s",
      "wait needy-actor-0123456789ab",
      "remove needy-actor-0123456789ab"
    ]
  );
}
