use std::path::{Path, PathBuf};
use std::time::Duration;

use needy_api_server::control::Sessions;

use crate::testing::{self, fake_image, new_actor_session, running};

const PROFILE: &str = r#"
user: { name: agent, uid: 1000, gid: 1000 }
executors:
  here: { runtime: native }
commands:
  - { match: "**", to: here }
"#;

/// A Firecracker that records its process ID next to `vm.json` and runs
/// until it is killed.
const LONG_RUNNING: &str = r#"echo $$ > "$(dirname "$3")/firecracker.pid"; exec sleep 300"#;

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
