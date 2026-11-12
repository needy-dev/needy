//! What holds for every cell, whatever it ran: private sockets, and
//! nothing left behind once its daemon stopped (containers, VMs, processes,
//! mounts, sockets).

use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::Path;
use std::time::Duration;

use nix::sys::signal::Signal;

use super::checks::Checks;
use super::env::CellEnv;
use super::proc;

/// Every socket under the cell is reachable by the daemon's user only:
/// the socket itself, or a directory on the way to it inside the cell, has
/// no permission bits for group or others. The control socket is 0600 in a
/// 0700 directory.
pub fn sockets(env: &CellEnv, checks: &mut Checks, when: &str) {
  let mut open = Vec::new();
  let mut count = 0;
  for socket in find_sockets(&env.dir) {
    count += 1;
    let private = |path: &Path| std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o077 == 0);
    let guarded = private(&socket) || socket.ancestors().skip(1).take_while(|d| d.starts_with(&env.dir) && *d != env.dir).any(private);
    if !guarded {
      let mode = std::fs::metadata(&socket).map(|m| m.permissions().mode() & 0o7777).unwrap_or(0);
      open.push(format!("{} ({mode:o})", socket.strip_prefix(&env.dir).unwrap_or(&socket).display()));
    }
  }
  checks.check(format!("sockets {when}: none reachable by other users"), open.is_empty(), format!("{count} sockets; open: {open:?}"));
  let control = env.runtime().join("daemon.sock");
  if when == "at daemon start" {
    let mode = |p: &Path| std::fs::metadata(p).map(|m| m.permissions().mode() & 0o777).ok();
    checks.eq("control socket mode", Some(0o600), mode(&control));
    checks.eq("runtime directory mode", Some(0o700), mode(&env.runtime()));
  }
}

fn find_sockets(dir: &Path) -> Vec<std::path::PathBuf> {
  let mut found = Vec::new();
  let mut stack = vec![dir.to_path_buf()];
  while let Some(dir) = stack.pop() {
    let Ok(entries) = std::fs::read_dir(&dir) else {
      continue;
    };
    for entry in entries.flatten() {
      let Ok(kind) = entry.file_type() else {
        continue;
      };
      if kind.is_dir() {
        stack.push(entry.path());
      } else if kind.is_socket() {
        found.push(entry.path());
      }
    }
  }
  found
}

/// Stops the cell's daemon and checks that nothing of the cell is left:
/// no process whose command line names the cell's directory (VMs, sshfs,
/// SFTP servers, executors), no container of its runs, no mount under it,
/// no socket. What is found is reported and then removed, so the next cell
/// starts clean.
pub fn after(env: &mut CellEnv, checks: &mut Checks) {
  if let Err(e) = env.stop_daemon() {
    checks.check("the daemon stops on SIGTERM", false, e);
  }
  let needle = env.dir.display().to_string();
  // Processes the daemon started end with it or just after (a VM process
  // that powers off, a container runtime's shim).
  let mut left = Vec::new();
  for _ in 0..50 {
    left = proc::processes_with(&needle);
    if left.is_empty() {
      break;
    }
    std::thread::sleep(Duration::from_millis(200));
  }
  checks.check("no leftover processes", left.is_empty(), format!("{left:?}"));
  for (pid, _) in &left {
    proc::signal(*pid, Signal::SIGKILL);
  }

  if proc::on_path("docker") {
    let mut containers = Vec::new();
    for run in &env.runs {
      let filter = format!("label=needy.run={run}");
      if let Ok(out) = proc::simple("docker", &["ps", "-a", "-q", "--filter", &filter], Duration::from_secs(30)) {
        containers.extend(out.stdout.split_whitespace().map(str::to_string));
      }
    }
    checks.check("no leftover containers in the engine", containers.is_empty(), format!("{containers:?}"));
    for id in containers {
      // Removal is the cleanup of a failure already recorded.
      let _ = proc::simple("docker", &["rm", "-f", &id], Duration::from_secs(60));
    }
  }

  if cfg!(target_os = "macos") && proc::on_path("container") {
    let names: Vec<String> = env.runs.iter().map(|r| format!("needy-actor-{r}")).collect();
    let listed = proc::simple("container", &["ls", "-a", "-q"], Duration::from_secs(30)).map(|o| o.stdout).unwrap_or_default();
    let found: Vec<&String> = names.iter().filter(|n| listed.split_whitespace().any(|l| l == n.as_str())).collect();
    checks.check("no leftover containers in Apple's container", found.is_empty(), format!("{found:?}"));
    for name in found {
      let _ = proc::simple("container", &["delete", "--force", name], Duration::from_secs(60));
    }
  }

  let mounts = proc::simple("mount", &[], Duration::from_secs(10)).map(|o| o.stdout).unwrap_or_default();
  let mounted: Vec<&str> = mounts.lines().filter(|l| l.contains(&needle)).collect();
  checks.check("no leftover mounts", mounted.is_empty(), format!("{mounted:?}"));
  for line in &mounted {
    if let Some(target) = line.split(" on ").nth(1).and_then(|r| r.split(' ').next()) {
      let _ = if cfg!(target_os = "linux") {
        proc::simple("fusermount3", &["-u", "-z", target], Duration::from_secs(30))
      } else {
        proc::simple("umount", &["-f", target], Duration::from_secs(30))
      };
    }
  }

  let sockets: Vec<String> = find_sockets(&env.dir).iter().map(|s| s.strip_prefix(&env.dir).unwrap_or(s).display().to_string()).collect();
  checks.check("no leftover sockets once the daemon stopped", sockets.is_empty(), format!("{sockets:?}"));
}
