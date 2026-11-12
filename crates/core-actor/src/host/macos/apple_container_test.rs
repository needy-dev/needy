use std::sync::Mutex;

use needy_config::Source;
use needy_config::profile::User;
use needy_core::recipe::{CompiledRecipe, HostInfo};

use super::*;

/// Apple's `container` CLI that records what it is asked: each command
/// succeeds unless `failing` names its first two words, and a started
/// `container run` exits with `code`.
#[derive(Default)]
struct Fake {
  calls: Mutex<Vec<String>>,
  failing: Vec<(&'static str, &'static str)>,
  code: i32,
}

impl Fake {
  fn calls(&self) -> Vec<String> {
    self.calls.lock().unwrap().clone()
  }
}

#[async_trait::async_trait]
impl Cli for Fake {
  async fn output(&self, args: &[String]) -> Result<Output, Error> {
    self.calls.lock().unwrap().push(args.join(" "));
    let failed = self.failing.iter().any(|(a, b)| args.first().map(String::as_str) == Some(a) && args.get(1).map(String::as_str) == Some(b));
    Ok(Output {
      success: !failed,
      stdout: String::new(),
      stderr: if failed {
        "Error: not found\n".into()
      } else {
        String::new()
      },
    })
  }

  fn spawn(&self, args: &[String], _log: std::fs::File) -> Result<Box<dyn Process>, Error> {
    self.calls.lock().unwrap().push(args[..3].join(" "));
    Ok(Box::new(Exits(self.code)))
  }
}

/// A `container run` process that exits with its code when waited for.
struct Exits(i32);

#[async_trait::async_trait]
impl Process for Exits {
  fn id(&self) -> u32 {
    4242
  }

  async fn wait(&mut self) -> std::io::Result<ExitStatus> {
    use std::os::unix::process::ExitStatusExt;
    Ok(ExitStatus::from_raw(self.0 << 8))
  }

  fn kill(&mut self) -> std::io::Result<()> {
    Ok(())
  }
}

fn runtime(fake: Fake) -> (MacosAppleContainer, Arc<Fake>) {
  let fake = Arc::new(fake);
  (MacosAppleContainer::with_cli("needy-actor:latest".into(), fake.clone()), fake)
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
  }
}

/// The value after each `flag` in `args`.
fn values<'a>(args: &'a [String], flag: &str) -> Vec<&'a str> {
  args.windows(2).filter(|w| w[0] == flag).map(|w| w[1].as_str()).collect()
}

#[test]
fn the_guest_and_gateway_sockets_are_mounted_one_file_each_and_the_process_socket_is_published() {
  let (runtime, _) = runtime(Fake::default());
  let dir = Path::new("/Users/user/.local/state/needy/runs/0123456789ab");
  let user = User::default();

  let launch = runtime.launch(&start(dir, &user, None));

  assert_eq!(
    values(&launch.args, "--volume"),
    [
      "/Users/user/.local/state/needy/runs/0123456789ab/channels/guest.sock:/.needy/channels/guest.sock",
      "/Users/user/.local/state/needy/runs/0123456789ab/channels/gateway.sock:/.needy/channels/gateway.sock",
    ]
  );
  assert_eq!(
    values(&launch.args, "--publish-socket"),
    ["/Users/user/.local/state/needy/runs/0123456789ab/channels/process.sock:/.needy/channels/process.sock"]
  );
  let channels = runtime.channels(dir);
  assert_eq!(channels.guest, Endpoint::Unix(dir.join("channels/guest.sock")));
  assert_eq!(channels.gateway, Endpoint::Unix(dir.join("channels/gateway.sock")));
  assert_eq!(channels.terminal, Terminal::Unix(dir.join("channels/process.sock")));
  assert!(channels.guest_for_every_user, "the driving program's proxy runs as the profile's user");
}

#[test]
fn the_actor_has_no_network_runs_init_and_is_named_after_its_run() {
  let (runtime, _) = runtime(Fake::default());
  let user = User::default();

  let launch = runtime.launch(&start(Path::new("/r"), &user, None));

  assert_eq!(launch.args[..3], ["run", "--name", "needy-actor-0123456789ab"]);
  assert_eq!(values(&launch.args, "--network"), ["none"]);
  assert_eq!(values(&launch.args, "--entrypoint"), ["/.needy/init"]);
  assert_eq!(values(&launch.args, "--label"), ["needy.run=0123456789ab", "needy.role=actor"]);
  assert_eq!(launch.args.last().map(String::as_str), Some("needy-actor:latest"), "the image comes last");
  assert_eq!(launch.name, "needy-actor-0123456789ab");
}

// ROOT CAUSE:
//
// If the root file system was read-only with a tmpfs over the channels
// directory, `run attach` failed with `transport error`, because `vminitd`
// reaches a published socket at the container's root file system path in
// the VM, outside the container's mounts, and found no socket there.
//
// The fix keeps the root file system writable and the channels directory
// on it.
#[test]
fn the_root_file_system_stays_writable_so_init_can_publish_its_process_socket() {
  let (runtime, _) = runtime(Fake::default());
  let user = User::default();

  let launch = runtime.launch(&start(Path::new("/r"), &user, None));

  assert!(!launch.args.iter().any(|a| a == "--read-only"), "{:?}", launch.args);
  assert!(!values(&launch.args, "--tmpfs").iter().any(|t| t.starts_with("/.needy")), "{:?}", launch.args);
}

#[test]
fn init_may_mount_the_workspace_and_set_the_hostname_inside() {
  let (runtime, _) = runtime(Fake::default());
  let user = User::default();

  let launch = runtime.launch(&start(Path::new("/r"), &user, None));

  assert_eq!(values(&launch.args, "--cap-add"), ["CAP_SYS_ADMIN"]);
  assert_eq!(runtime.setup(), Setup::GuestMounts);
  assert!(runtime.setup().mounts_in_guest() && !runtime.setup().identity_in_guest());
}

#[test]
fn the_home_directory_is_a_tmpfs_the_user_owns() {
  let (runtime, _) = runtime(Fake::default());
  let user = User {
    name: "dev".into(),
    uid: 1001,
    gid: 1002,
  };

  let launch = runtime.launch(&start(Path::new("/r"), &user, None));

  assert_eq!(
    values(&launch.args, "--tmpfs"),
    [
      "/tmp:mode=1777",
      "/var/tmp:mode=1777",
      "/run:mode=755",
      "/home/dev:mode=755,uid=1001,gid=1002"
    ]
  );
}

#[test]
fn a_recipe_sets_the_cpus_and_memory() {
  let (runtime, _) = runtime(Fake::default());
  let recipe = gce();
  let user = User::default();

  let launch = runtime.launch(&start(Path::new("/r"), &user, Some(&recipe)));

  assert_eq!((launch.vcpus, launch.mem_mib), (2, 8192));
  assert_eq!(values(&launch.args, "--cpus"), ["2"]);
  assert_eq!(values(&launch.args, "--memory"), ["8192M"]);
}

#[tokio::test]
async fn a_started_actor_is_waited_for_through_its_cli_process_and_deleted() {
  let dir = tempfile::tempdir().unwrap();
  let (runtime, fake) = runtime(Fake {
    code: 3,
    ..Default::default()
  });
  let user = User::default();

  let mut actor = runtime.start(&start(dir.path(), &user, None)).await.unwrap();
  let status = actor.wait().await.unwrap();

  assert_eq!(status.code(), Some(3));
  assert_eq!(actor.pid(), 4242);
  assert!(dir.path().join("console.log").exists(), "the CLI's output is the actor's log");
  let name = "needy-actor-0123456789ab";
  assert_eq!(
    fake.calls(),
    [
      format!("delete --force {name}"),
      format!("run --name {name}"),
      format!("delete --force {name}")
    ]
  );
}

#[tokio::test]
async fn stopping_an_actor_stops_its_container_within_the_grace_and_deletes_it() {
  let dir = tempfile::tempdir().unwrap();
  let (runtime, fake) = runtime(Fake {
    code: 143,
    ..Default::default()
  });
  let user = User::default();

  let mut actor = runtime.start(&start(dir.path(), &user, None)).await.unwrap();
  let status = actor.stop(Duration::from_secs(3)).await.unwrap();

  assert_eq!(status.code(), Some(143));
  assert_eq!(
    fake.calls()[2..],
    [
      "stop --time 3 needy-actor-0123456789ab",
      "delete --force needy-actor-0123456789ab"
    ]
  );
  // A second stop or wait does not touch the container again.
  actor.wait().await.unwrap();
  assert_eq!(fake.calls().len(), 4);
}

#[tokio::test]
async fn a_leftover_container_is_deleted_by_its_run_name() {
  let (runtime, fake) = runtime(Fake::default());

  assert!(runtime.end_leftover("0123456789ab").await);
  assert_eq!(fake.calls(), ["delete --force needy-actor-0123456789ab"]);

  let (runtime, _) = runtime_failing(&[("delete", "--force")]);
  assert!(!runtime.end_leftover("0123456789ab").await, "no container was there");
}

fn runtime_failing(failing: &[(&'static str, &'static str)]) -> (MacosAppleContainer, Arc<Fake>) {
  runtime(Fake {
    failing: failing.to_vec(),
    ..Default::default()
  })
}

#[tokio::test]
async fn services_that_do_not_answer_name_the_command_that_starts_them() {
  let (runtime, _) = runtime_failing(&[("system", "status")]);

  let error = runtime.check_host().await.unwrap_err().to_string();

  assert!(error.contains("Apple's `container` services are not running (Error: not found)"), "{error}");
  assert!(error.contains("container system start"), "{error}");
}

#[tokio::test]
async fn a_missing_image_names_the_build_that_makes_it() {
  let (runtime, fake) = runtime_failing(&[("image", "inspect")]);

  let error = runtime.check_host().await.unwrap_err().to_string();

  assert!(error.contains("needy-actor:latest is not in Apple's `container` image store"), "{error}");
  assert!(error.contains("needy build --macos-apple-container"), "{error}");
  assert_eq!(fake.calls(), ["system status", "image inspect needy-actor:latest"]);
}

#[tokio::test]
async fn running_services_and_the_image_pass_the_host_check() {
  let (runtime, _) = runtime(Fake::default());

  runtime.check_host().await.unwrap();
}

#[test]
fn a_plain_machine_lacks_sensors_and_a_cpu_count_without_the_agent_s() {
  let (runtime, _) = runtime(Fake::default());

  let gaps = runtime.identity_gaps(None);

  let fields: Vec<_> = gaps.iter().map(|g| g.field).collect();
  assert_eq!(fields, [None, Some(IdentityField::Hwmon)]);
}

#[test]
fn a_recipe_s_cpu_dmi_and_kernel_are_gaps_and_its_hostname_is_not() {
  let (runtime, _) = runtime(Fake::default());
  let recipe = gce();

  let fields: Vec<_> = runtime.identity_gaps(Some(&recipe)).into_iter().filter_map(|g| g.field).collect();

  for field in [
    IdentityField::Cpuinfo,
    IdentityField::Cpuid,
    IdentityField::Dmi,
    IdentityField::Hwmon,
  ] {
    assert!(fields.contains(&field), "{field:?} missing from {fields:?}");
  }
  assert!(!fields.contains(&IdentityField::Hostname), "init sets the hostname: {fields:?}");
}

#[tokio::test]
async fn an_image_archive_is_loaded_into_the_image_store() {
  let (runtime, fake) = runtime(Fake::default());

  runtime.load(Path::new("/tmp/needy-actor.tar")).await.unwrap();

  assert_eq!(fake.calls(), ["image load --input /tmp/needy-actor.tar"]);
  let (runtime, _) = runtime_failing(&[("image", "load")]);
  let error = runtime.load(Path::new("/tmp/needy-actor.tar")).await.unwrap_err().to_string();
  assert_eq!(error, "`container image load` failed: Error: not found");
}
