//! Last-resort cleanup restricted to one fixture's unique temporary path.

use super::*;

#[derive(Debug)]
pub(super) struct Process {
  pub(super) pid: i32,
  pub(super) parent: i32,
  pub(super) kind: &'static str,
}

pub(super) fn list(home: &Path) -> std::io::Result<Vec<Process>> {
  let output = Command::new("ps").args(["-axo", "pid=,ppid=,command="]).output()?;
  if !output.status.success() {
    return Err(std::io::Error::other("cannot inspect fixture processes"));
  }
  Ok(
    String::from_utf8(output.stdout)
      .map_err(std::io::Error::other)?
      .lines()
      .filter_map(|line| {
        let mut fields = line.split_whitespace();
        let pid = fields.next()?.parse().ok()?;
        let parent = fields.next()?.parse().ok()?;
        let command = fields.collect::<Vec<_>>().join(" ");
        if !command.contains(home.to_str().unwrap()) {
          return None;
        }
        let kind = if command.contains("daemon holder --config-file") {
          "runtime-holder"
        } else if command.contains("run execution-holder --startup-stdin") {
          "execution-holder"
        } else if command.contains("image/firecracker") && command.starts_with("/bin/sh ") {
          "actor"
        } else if command.contains("daemon serve") {
          "control"
        } else {
          return None;
        };
        Some(Process { pid, parent, kind })
      })
      .collect(),
  )
}

/// Cleans up only processes launched with this fixture's unique directory.
pub(super) struct Cleanup(pub(super) PathBuf);

impl Cleanup {
  fn stop(&self) -> std::io::Result<()> {
    let owned = list(&self.0)?;
    for process in &owned {
      // SAFETY: these PIDs were just selected by this fixture's unique path.
      unsafe { libc::kill(process.pid, libc::SIGTERM) };
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    while !list(&self.0)?.is_empty() && Instant::now() < deadline {
      std::thread::sleep(Duration::from_millis(20));
    }
    for process in list(&self.0)? {
      // SAFETY: only this fixture's remaining processes are selected.
      unsafe { libc::kill(process.pid, libc::SIGKILL) };
    }
    for process in owned.iter().filter(|p| p.parent == std::process::id() as i32) {
      // SAFETY: reap only direct children this fixture started. A readiness
      // panic dropped Child before a normal owner could wait for the control.
      unsafe { libc::waitpid(process.pid, std::ptr::null_mut(), 0) };
    }
    Ok(())
  }
}

impl Drop for Cleanup {
  fn drop(&mut self) {
    if let Err(error) = self.stop() {
      // Fallback cleanup must not double-panic during a failed assertion.
      eprintln!("cannot clean fixture processes: {error}");
    }
  }
}
