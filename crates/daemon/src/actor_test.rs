use std::path::{Path, PathBuf};
use std::time::Duration;

use needy_api_server::control::Runs;
use needy_core::types::RunState;

use crate::testing::{self, fake_image, new_run, running};

const PROFILE: &str = r#"
actor: { driver: linux-firecracker }
user: { name: agent, uid: 1000, gid: 1000 }
executors:
  here: { driver: native }
commands:
  - { match: "**", to: here }
"#;

/// A Firecracker that creates its vsock socket's file, records its process
/// ID next to `vm.json` and runs until it is killed.
const LONG_RUNNING: &str = r#"dir=$(dirname "$3"); touch "$dir/v.sock"; echo $$ > "$dir/firecracker.pid"; exec sleep 300"#;

/// The names of the socket files in `dir`.
fn sockets(dir: &Path) -> Vec<String> {
  let mut names: Vec<String> = std::fs::read_dir(dir)
    .unwrap()
    .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
    .filter(|n| n.starts_with("v.sock"))
    .collect();
  names.sort();
  names
}

/// Waits for the stand-in Firecracker of the run in `dir` to write its
/// process ID.
async fn firecracker_pid(dir: &Path) -> i64 {
  let path = dir.join("firecracker.pid");
  for _ in 0..100 {
    if let Ok(text) = std::fs::read_to_string(&path)
      && let Ok(pid) = text.trim().parse()
    {
      return pid;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
  }
  panic!("{} was not written", path.display());
}

fn run_dir(started: &testing::Started, id: &str) -> PathBuf {
  started.dir.path().join("state/runs").join(id)
}

// ROOT CAUSE:
//
// If a run whose VM was still running was stopped, Firecracker kept
// running because the VM watch returned on cancellation and dropped the
// `Vm` handle, which does not kill its child.
//
// Before the fix, `run stop` left the firecracker process behind.
// The fix keeps the VM's life inside its run's: the stop kills
// Firecracker through the handle and waits for it.
#[tokio::test]
async fn stopping_a_run_stops_its_vm() {
  let started = testing::daemon().await;
  fake_image(&started.dir.path().join("image"), LONG_RUNNING);
  let created = Runs::create(&started.daemon, new_run(PROFILE, started.dir.path())).await.unwrap();
  let pid = firecracker_pid(&run_dir(&started, &created.id)).await;
  assert!(running(pid));

  tokio::time::timeout(Duration::from_secs(20), Runs::stop(&started.daemon, &created.id, needy_core::types::Principal::Operator))
    .await
    .unwrap()
    .unwrap();

  assert!(!running(pid), "firecracker {pid} outlived its run");
}

// ROOT CAUSE:
//
// If a run with an actor was stopped, its guest listener kept serving
// and the actor's sockets stayed in the run's directory, because the
// listener ran under the daemon's token and tracker and nothing removed the
// socket files.
//
// Before the fix, `v.sock`, `v.sock_7000` and `v.sock_7001` were left behind
// and the daemon held one listener task per stopped run until it exited.
// The fix runs the guest listener under the actor's token in the run's
// tracker and removes the sockets when the run stops.
#[tokio::test]
async fn stopping_a_run_stops_its_guest_listener_and_removes_its_sockets() {
  let started = testing::daemon().await;
  fake_image(&started.dir.path().join("image"), LONG_RUNNING);
  let daemon_tasks = started.daemon.tasks.len();
  let created = Runs::create(&started.daemon, new_run(PROFILE, started.dir.path())).await.unwrap();
  let dir = run_dir(&started, &created.id);
  firecracker_pid(&dir).await;
  assert_eq!(sockets(&dir), ["v.sock", "v.sock_7000", "v.sock_7001"]);

  tokio::time::timeout(Duration::from_secs(20), Runs::stop(&started.daemon, &created.id, needy_core::types::Principal::Operator))
    .await
    .unwrap()
    .unwrap();

  assert_eq!(sockets(&dir), Vec::<String>::new());
  assert_eq!(started.daemon.tasks.len(), daemon_tasks, "the run's tasks are not the daemon's");
}

// ROOT CAUSE:
//
// If a run's actor exited on its own, the run kept its executors
// and the actor's sockets until someone ran `run stop`, because the VM
// watch only recorded the exit and marked the run finished.
//
// Before the fix, `examples/smoke` left its Docker executor running after
// the driving program finished.
// The fix keeps a finished run holding nothing: when the VM exits, the
// run mirrors out, releases its executors and removes the actor's
// sockets, and it stays listed as finished until it is stopped.
#[tokio::test]
async fn an_actor_that_exits_releases_its_executors_and_sockets() {
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  fake_image(&started.dir.path().join("image"), testing::REBOOTS_AFTER_EXIT);
  let created = Runs::create(daemon, new_run(PROFILE, started.dir.path())).await.unwrap();
  let dir = run_dir(&started, &created.id);
  testing::exit_actor(&started.daemon, started.dir.path(), &created.id).await;

  let released = async {
    while !(daemon.pool.instances().await.is_empty() && sockets(&dir).is_empty()) {
      tokio::time::sleep(Duration::from_millis(20)).await;
    }
  };
  tokio::time::timeout(Duration::from_secs(10), released).await.expect("the finished run still holds its executors or sockets");

  let listed = Runs::list(daemon).await.unwrap();
  assert_eq!(listed[0].state, RunState::Finished);
  Runs::stop(daemon, &created.id, needy_core::types::Principal::Operator).await.unwrap();
  assert_eq!(Runs::get(daemon, &created.id).await.unwrap().state, RunState::Finished);
}

// ROOT CAUSE:
//
// If the actor's VM exited without its driving program having reported an
// exit, such as a Firecracker that refused the recipe's CPU template, the
// run was `finished` with no findings, because the VM watch finished
// every run whose VM exited.
//
// Before the fix, `run ls` and `report` said `finished` and only
// `run logs` showed why.
// The fix ends such a run `failed`, with the VM's exit status and the
// end of its log as the reason, recorded as `run.fail`.
#[tokio::test]
async fn a_vm_that_exits_before_its_driving_program_fails_the_run_with_the_reason() {
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  fake_image(
    &started.dir.path().join("image"),
    r#"echo "Running Firecracker"; echo "Error: Template changes a CPUID entry not supported by KVM: Leaf: 7, Subleaf: 1"; exit 1"#,
  );
  let created = Runs::create(daemon, new_run(PROFILE, started.dir.path())).await.unwrap();

  // The run ends, then gives back its executors.
  let ended = async {
    loop {
      let info = Runs::get(daemon, &created.id).await.unwrap();
      let open = matches!(info.state, RunState::Creating | RunState::Running);
      if !open && daemon.pool.instances().await.is_empty() {
        return info;
      }
      tokio::time::sleep(Duration::from_millis(20)).await;
    }
  };
  let info = tokio::time::timeout(Duration::from_secs(10), ended).await.expect("the run did not end and release its executors");

  assert_eq!(info.state, RunState::Failed);
  let failure = info.failure.unwrap_or_default();
  assert!(failure.starts_with("the actor exited before its driving program did: linux-firecracker exited with code 1"), "{failure}");
  assert!(failure.contains("Template changes a CPUID entry not supported by KVM: Leaf: 7, Subleaf: 1"), "{failure}");

  // A stop keeps the failure and flushes the trace.
  Runs::stop(daemon, &created.id, needy_core::types::Principal::Operator).await.unwrap();
  assert_eq!(Runs::get(daemon, &created.id).await.unwrap().state, RunState::Failed);
  let trace = std::fs::read_to_string(run_dir(&started, &created.id).join("trace.jsonl")).unwrap();
  assert!(trace.contains("\"run.fail\""), "{trace}");
}

// ROOT CAUSE:
//
// If the host had no `/dev/kvm`, `run create` of a profile with an actor
// exited 0, because the daemon started Firecracker, which exited at once,
// and the run then read as `finished`.
//
// Before the fix, the missing KVM showed only in `run logs`.
// The fix checks the KVM device before anything starts and fails the create
// with a message that names it.
#[tokio::test]
async fn an_actor_on_a_host_without_kvm_fails_the_create_at_once() {
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  fake_image(&started.dir.path().join("image"), LONG_RUNNING);
  let kvm = started.dir.path().join("kvm");
  std::fs::remove_file(&kvm).unwrap();

  let created = tokio::time::timeout(Duration::from_secs(5), Runs::create(daemon, new_run(PROFILE, started.dir.path())))
    .await
    .expect("the create fails at once");

  let Err(needy_api_server::control::ControlError::Failed(message)) = created else {
    panic!("a create without KVM must fail");
  };
  assert!(message.contains(&format!("this host has no usable KVM: cannot open {}", kvm.display())), "{message}");
  // A user with only the binary is told what works without KVM, not
  // pointed at a design note in the repository.
  assert!(
    message.contains("Without KVM, use a linux-container actor (`actor: { driver: linux-container, container: { oci_runtime: runsc } }`); commands outside runs also work: `needy exec --runner`, `ps`, `attach`, `kill` and `fs cp`"),
    "{message}"
  );
  assert!(!message.contains("docs/"), "{message}");
  // Seen on a cloud host without KVM: the OS error was printed twice, once
  // in the message and once as its cause.
  assert_eq!(message.matches("(os error 2)").count(), 1, "{message}");
  let listed = Runs::list(daemon).await.unwrap();
  assert_eq!(listed[0].state, RunState::Failed);
  assert!(!run_dir(&started, &listed[0].id).join("firecracker.pid").exists(), "firecracker was started");
  assert!(daemon.pool.instances().await.is_empty());
}

// ROOT CAUSE:
//
// If the host had no KVM, a create with executors acquired them first (for
// Docker, creating containers or failing on a missing image) and checked KVM
// only when it started the actor, because the check sat in the actor's
// start, after the executors.
//
// Before the fix, `examples/smoke` on a host without KVM and without the
// executor image failed on the image, not on KVM.
// The fix checks KVM before the run acquires anything.
#[tokio::test]
async fn a_create_without_kvm_fails_before_it_acquires_executors() {
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  fake_image(&started.dir.path().join("image"), LONG_RUNNING);
  std::fs::remove_file(started.dir.path().join("kvm")).unwrap();
  // Acquiring `far` fails: the operator allows no privileged container.
  let profile = r#"
actor: { driver: linux-firecracker }
user: { name: agent, uid: 1000, gid: 1000 }
executors:
  far: { driver: linux-container, container: { privileged: true } }
commands:
  - { match: "**", to: far }
"#;

  let Err(needy_api_server::control::ControlError::Failed(message)) = Runs::create(daemon, new_run(profile, started.dir.path())).await
  else {
    panic!("a create without KVM must fail");
  };

  assert!(message.contains("this host has no usable KVM"), "{message}");
}

// ROOT CAUSE:
//
// If `run attach` reached a macos-vz actor before its guest served the process
// service, vfkit accepted the connection on `process.sock` and closed it once
// it could not reach the guest's vsock port, and the attach failed with
// "transport error" instead of waiting, because a Unix connection that was
// accepted counted as the terminal.
//
// The fix counts the terminal only once the service answers a call.
#[tokio::test]
async fn a_relay_that_accepts_and_closes_is_no_terminal_yet() {
  let dir = tempfile::tempdir().unwrap();
  let path = dir.path().join("process.sock");
  let listener = tokio::net::UnixListener::bind(&path).unwrap();
  // Like vfkit with no guest listening: accept, then close.
  let relay = tokio::spawn(async move {
    loop {
      let (stream, _) = listener.accept().await.unwrap();
      drop(stream);
    }
  });

  let connected = super::connect_terminal(&path).await;
  relay.abort();

  assert!(connected.is_err(), "a closed relay counted as the terminal");
}

#[tokio::test]
async fn a_served_process_service_is_the_terminal() {
  let dir = tempfile::tempdir().unwrap();
  let path = dir.path().join("process.sock");
  let health = needy_api_server::Health {
    instance_id: "test".into(),
    role: "executor",
  };
  let listener = needy_api_server::Server::new(health, needy_api_server::control::Caller::Operator)
    .bind(&[needy_api_server::Listen::Unix(path.clone())])
    .await
    .unwrap();
  let stop = tokio_util::sync::CancellationToken::new();
  let served = tokio::spawn(listener.serve(stop.clone()));

  let connected = super::connect_terminal(&path).await;
  stop.cancel();
  served.await.unwrap().unwrap();

  assert!(connected.is_ok(), "{:?}", connected.err());
}
