//! The processes a routed command runs under, read from `/proc`, for the
//! rules' `when.under`.

use std::path::Path;

use procfs_core::FromRead;
use procfs_core::process::Stat;

/// Ancestors read before the walk stops.
// NOTICE: a real chain in an actor is a few processes deep; the limit only
// ends a walk that a corrupt or racing `/proc` would keep going.
const MAX_DEPTH: usize = 64;

/// The argument vectors of `pid`'s ancestors under `proc` (a `/proc`
/// directory), nearest first, up to but not including init (PID 1). A
/// process that exits during the walk ends it: the rules see the chain read
/// so far.
pub(super) fn ancestors(proc: &Path, pid: i32) -> Vec<Vec<String>> {
  let mut chain = Vec::new();
  let mut pid = pid;
  while chain.len() < MAX_DEPTH {
    let Ok(stat) = Stat::from_file(proc.join(pid.to_string()).join("stat")) else {
      break;
    };
    pid = stat.ppid;
    if pid <= 1 {
      break;
    }
    // `cmdline` is NUL-terminated arguments; procfs-core parses only
    // fixed-format files, so the split is here.
    let Ok(cmdline) = std::fs::read(proc.join(pid.to_string()).join("cmdline")) else {
      break;
    };
    let args = cmdline.strip_suffix(&[0]).unwrap_or(&cmdline);
    // A zombie's `cmdline` is empty.
    let argv = if args.is_empty() {
      Vec::new()
    } else {
      args.split(|b| *b == 0).map(|a| String::from_utf8_lossy(a).into_owned()).collect()
    };
    chain.push(argv);
  }
  chain
}

#[cfg(test)]
#[path = "ancestry_test.rs"]
mod tests;
