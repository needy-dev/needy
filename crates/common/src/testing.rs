//! Helpers for other crates' tests (feature `testing`, Unix).

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

/// Writes `text` to an executable file at `path` (mode 0755) that a test
/// then runs, without this process ever holding a descriptor that writes it.
// NOTICE(etxtbsy): Linux refuses to run a file that any process has open for
// writing (`ETXTBSY`, "Text file busy", `execve(2)`). A test binary runs
// tests on many threads. A fork in one thread (a test or the code under test
// spawning a program) copies every open descriptor, also the one another
// thread is writing a script through, and the child keeps it until its own
// exec closes it (`O_CLOEXEC`). A script run in that window fails to start.
// Writing to a temporary file and renaming it does not help: the busy file is
// the same inode. Here a child process writes the file, so no fork of the
// test process can copy a descriptor of it, and once the child has exited
// nothing has it open for writing. The fix needs no retry in the product,
// which never runs a file it has just written in the same process.
// Reference: https://man7.org/linux/man-pages/man2/execve.2.html (ETXTBSY)
pub fn write_executable(path: &Path, text: &str) -> std::io::Result<()> {
  let mut child = Command::new("sh").args(["-c", "cat > \"$1\" && chmod 755 \"$1\"", "sh"]).arg(path).stdin(Stdio::piped()).spawn()?;
  let written = child.stdin.take().expect("stdin is piped").write_all(text.as_bytes());
  let status = child.wait()?;
  written?;
  if !status.success() {
    return Err(std::io::Error::other(format!("cannot write {}: sh exited with {status}", path.display())));
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use std::os::unix::fs::PermissionsExt;

  #[test]
  fn an_executable_written_by_a_child_runs() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("script");

    super::write_executable(&script, "#!/bin/sh\necho ran\n").unwrap();

    assert_eq!(std::fs::metadata(&script).unwrap().permissions().mode() & 0o777, 0o755);
    let output = std::process::Command::new(&script).output().unwrap();
    assert_eq!(output.stdout, b"ran\n");
  }

  // ROOT CAUSE:
  //
  // If a test wrote a script and ran it while another thread of the test
  // binary forked, the run failed with "Text file busy" (`ETXTBSY`), because
  // the forked child held a copy of the descriptor the script was written
  // through until its own exec.
  //
  // Before the fix, the stand-in Firecracker tests wrote their scripts with
  // `std::fs::write` and failed now and then on Linux under load.
  // The fix writes scripts from a child process, so no fork of the test
  // process can hold them open for writing.
  #[cfg(target_os = "linux")]
  #[test]
  fn a_script_written_while_another_thread_forks_is_never_busy() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    let dir = tempfile::tempdir().unwrap();
    let done = Arc::new(AtomicBool::new(false));
    let forks: Vec<_> = (0..4)
      .map(|_| {
        let done = done.clone();
        std::thread::spawn(move || {
          while !done.load(Ordering::Relaxed) {
            std::process::Command::new("true").status().unwrap();
          }
        })
      })
      .collect();

    let mut busy = 0;
    for i in 0..300 {
      let script = dir.path().join(format!("script-{i}"));
      super::write_executable(&script, "#!/bin/sh\nexit 0\n").unwrap();
      match std::process::Command::new(&script).status() {
        Ok(status) => assert!(status.success()),
        // ETXTBSY is 26 on Linux (`asm-generic/errno-base.h`).
        Err(e) if e.raw_os_error() == Some(26) => busy += 1,
        Err(e) => panic!("{e}"),
      }
    }
    done.store(true, Ordering::Relaxed);
    for fork in forks {
      fork.join().unwrap();
    }

    assert_eq!(busy, 0, "{busy} of 300 scripts were busy");
  }
}
