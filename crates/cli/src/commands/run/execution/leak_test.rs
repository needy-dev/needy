//! Probes the real holder's wait ownership without a transport or VM fixture.

use super::*;
use std::time::{Duration, Instant};

struct Reap(i32);

impl Drop for Reap {
  fn drop(&mut self) {
    // SAFETY: this test created this child and is its only remaining waiter.
    unsafe {
      let mut status = 0;
      if libc::waitpid(self.0, &mut status, libc::WNOHANG) == 0 {
        libc::kill(self.0, libc::SIGKILL);
        libc::waitpid(self.0, &mut status, 0);
      }
    }
  }
}

#[test]
fn unkept_holder_stops_and_reaps_its_child() {
  let directory = tempfile::tempdir().unwrap();
  let child = Command::new("sh").args(["-c", "exec sleep 30"]).spawn().unwrap();
  let pid = child.id() as i32;
  let _reap = Reap(pid);
  let holder = Holder {
    capability: String::new(),
    child: Some(child),
    directory: directory.path().to_path_buf(),
    kept: false,
  };
  drop(holder);
  let output = Command::new("ps").args(["-p", &pid.to_string(), "-o", "state="]).output().unwrap();
  assert!(String::from_utf8(output.stdout).unwrap().trim().is_empty(), "a failed creation must stop and reap its child");
}

// ROOT CAUSE:
// Before the fix, keep() discarded Child without transferring wait ownership.
// If the execution holder exited while its creating CLI was still alive, the
// child remained a zombie. The fix gives a waiter ownership of the child.
#[test]
fn kept_holder_is_reaped_while_the_creating_process_is_alive() {
  let directory = tempfile::tempdir().unwrap();
  let child = Command::new("sh").args(["-c", "exit 0"]).spawn().unwrap();
  let pid = child.id() as i32;
  let _reap = Reap(pid);
  let mut holder = Holder {
    capability: String::new(),
    child: Some(child),
    directory: directory.path().to_path_buf(),
    kept: false,
  };
  holder.keep();
  drop(holder);
  let deadline = Instant::now() + Duration::from_secs(2);
  loop {
    let output = Command::new("ps").args(["-p", &pid.to_string(), "-o", "state="]).output().unwrap();
    let state = String::from_utf8(output.stdout).unwrap();
    if state.trim().is_empty() {
      return;
    }
    if Instant::now() >= deadline {
      assert!(!state.trim().starts_with('Z'), "kept holder is still a zombie after two seconds: {}", state.trim());
      panic!("holder child has not exited after two seconds: {}", state.trim());
    }
    std::thread::sleep(Duration::from_millis(20));
  }
}
