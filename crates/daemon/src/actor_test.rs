use std::path::{Path, PathBuf};
use std::time::Duration;

use needy_api_server::control::Sessions;
use needy_core::types::SessionState;

use crate::testing::{self, fake_image, new_actor_session, running};

const PROFILE: &str = r#"
user: { name: agent, uid: 1000, gid: 1000 }
executors:
  here: { runtime: native }
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

/// Waits for the stand-in Firecracker of the session in `dir` to write its
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

fn session_dir(started: &testing::Started, id: &str) -> PathBuf {
  started.dir.path().join("state/sessions").join(id)
}

// ROOT CAUSE:
//
// If a session whose VM was still running was stopped, Firecracker kept
// running because the VM watch returned on cancellation and dropped the
// `Vm` handle, which does not kill its child.
//
// Before the fix, `session stop` left the firecracker process behind.
// The fix keeps the VM's life inside its session's: the stop kills
// Firecracker through the handle and waits for it.
#[tokio::test]
async fn stopping_a_session_stops_its_vm() {
  let started = testing::daemon().await;
  fake_image(&started.dir.path().join("image"), LONG_RUNNING);
  let created = Sessions::create(&started.daemon, new_actor_session(PROFILE, started.dir.path())).await.unwrap();
  let pid = firecracker_pid(&session_dir(&started, &created.id)).await;
  assert!(running(pid));

  tokio::time::timeout(Duration::from_secs(20), Sessions::stop(&started.daemon, &created.id)).await.unwrap().unwrap();

  assert!(!running(pid), "firecracker {pid} outlived its session");
}

// ROOT CAUSE:
//
// If a session with an actor was stopped, its guest listener kept serving
// and the actor's sockets stayed in the session's directory, because the
// listener ran under the daemon's token and tracker and nothing removed the
// socket files.
//
// Before the fix, `v.sock`, `v.sock_7000` and `v.sock_7001` were left behind
// and the daemon held one listener task per stopped session until it exited.
// The fix runs the guest listener under the actor's token in the session's
// tracker and removes the sockets when the session stops.
#[tokio::test]
async fn stopping_a_session_stops_its_guest_listener_and_removes_its_sockets() {
  let started = testing::daemon().await;
  fake_image(&started.dir.path().join("image"), LONG_RUNNING);
  let daemon_tasks = started.daemon.tasks.len();
  let created = Sessions::create(&started.daemon, new_actor_session(PROFILE, started.dir.path())).await.unwrap();
  let dir = session_dir(&started, &created.id);
  firecracker_pid(&dir).await;
  assert_eq!(sockets(&dir), ["v.sock", "v.sock_7000", "v.sock_7001"]);

  tokio::time::timeout(Duration::from_secs(20), Sessions::stop(&started.daemon, &created.id)).await.unwrap().unwrap();

  assert_eq!(sockets(&dir), Vec::<String>::new());
  assert_eq!(started.daemon.tasks.len(), daemon_tasks, "the session's tasks are not the daemon's");
}

// ROOT CAUSE:
//
// If a session's actor exited on its own, the session kept its executors
// and the actor's sockets until someone ran `session stop`, because the VM
// watch only recorded the exit and marked the session finished.
//
// Before the fix, `examples/smoke` left its Docker executor running after
// the driving program finished.
// The fix keeps a finished session holding nothing: when the VM exits, the
// session mirrors out, releases its executors and removes the actor's
// sockets, and it stays listed as finished until it is stopped.
#[tokio::test]
async fn an_actor_that_exits_releases_its_executors_and_sockets() {
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  fake_image(&started.dir.path().join("image"), r#"touch "$(dirname "$3")/v.sock"; sleep 0.2"#);
  let created = Sessions::create(daemon, new_actor_session(PROFILE, started.dir.path())).await.unwrap();
  let dir = session_dir(&started, &created.id);

  let released = async {
    while !(daemon.pool.instances().await.is_empty() && sockets(&dir).is_empty()) {
      tokio::time::sleep(Duration::from_millis(20)).await;
    }
  };
  tokio::time::timeout(Duration::from_secs(10), released).await.expect("the finished session still holds its executors or sockets");

  let listed = Sessions::list(daemon).await.unwrap();
  assert_eq!(listed[0].state, SessionState::Finished);
  Sessions::stop(daemon, &created.id).await.unwrap();
  assert_eq!(Sessions::get(daemon, &created.id).await.unwrap().state, SessionState::Finished);
}
