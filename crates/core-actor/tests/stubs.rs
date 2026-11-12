//! The base image's stubs (`images/stub/`), built with this machine's C
//! compiler against a directory of the test's own (`-DNEEDY_DIR`), and run
//! with a stand-in guest needy that prints what it was given. They are the
//! base ABI's hand-off, so these tests pin it: see
//! `docs/ai/references/architecture/2026-11-12-actor-images-design.md`.
//!
//! Linux only: the stubs read `/proc/self/exe` and the auxiliary vector.
#![cfg(target_os = "linux")]

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A stand-in guest needy: prints its argv and the stub variables.
const FAKE_GUEST: &str = r#"
#include <stdio.h>
#include <string.h>
extern char **environ;
int main(int argc, char **argv) {
  for (int i = 0; i < argc; i++) printf("argv[%d]=%s\n", i, argv[i]);
  for (char **e = environ; *e; e++) if (!strncmp(*e, "NEEDY_STUB_", 11)) printf("%s\n", *e);
  return 0;
}
"#;

struct Layout {
  _dir: tempfile::TempDir,
  /// The test's stand-in for `/.needy`.
  needy: PathBuf,
  boot: PathBuf,
  shim: PathBuf,
}

fn compile(source: &Path, out: &Path, needy: &Path) {
  let status = Command::new("cc")
    .arg(format!("-DNEEDY_DIR=\"{}\"", needy.display()))
    .arg("-DDISK=\"/nonexistent/vdb\"")
    .args(["-O2", "-Wall", "-Werror", "-o"])
    .arg(out)
    .arg(source)
    .status()
    .expect("a C compiler `cc` on PATH");
  assert!(status.success(), "cc {}: {status}", source.display());
}

fn layout() -> Layout {
  let dir = tempfile::tempdir().unwrap();
  let root = std::fs::canonicalize(dir.path()).unwrap();
  let needy = root.join(".needy");
  std::fs::create_dir_all(needy.join("disk")).unwrap();
  let stubs = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../images/stub");
  let boot = needy.join("init");
  let shim = needy.join("proxy");
  compile(&stubs.join("boot.c"), &boot, &needy);
  compile(&stubs.join("shim.c"), &shim, &needy);
  Layout {
    _dir: dir,
    needy,
    boot,
    shim,
  }
}

impl Layout {
  /// Injects the stand-in guest needy, as the daemon does.
  fn inject(&self) {
    let source = self.needy.join("guest.c");
    std::fs::write(&source, FAKE_GUEST).unwrap();
    compile(&source, &self.needy.join("needy"), &self.needy);
  }

  /// A program on PATH that is a shim: a hard link to the shim stub.
  fn shim_at(&self, path: &str) -> PathBuf {
    let path = self.needy.parent().unwrap().join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::hard_link(&self.shim, &path).unwrap();
    path
  }
}

fn stdout(output: &Output) -> String {
  String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
  String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn a_shim_hands_the_guest_needy_the_callers_argv_and_its_own_path() {
  let layout = layout();
  layout.inject();
  let tool = layout.shim_at("usr/bin/python3");

  let output = Command::new(&tool).arg0("python3").args(["-c", "pass"]).output().unwrap();

  assert!(output.status.success(), "{}", stderr(&output));
  assert_eq!(
    stdout(&output),
    format!("argv[0]=python3\nargv[1]=-c\nargv[2]=pass\nNEEDY_STUB_EXE={tool}\nNEEDY_STUB_EXECFN={tool}\n", tool = tool.display())
  );
}

#[test]
fn a_shim_started_through_a_symbolic_link_passes_the_link_as_the_exec_path() {
  // A route stub is a symbolic link to the proxy: `/proc/self/exe` names
  // the proxy, `AT_EXECFN` the link the caller ran.
  let layout = layout();
  layout.inject();
  let stub = layout.needy.parent().unwrap().join("run/needy/bin/cargo");
  std::fs::create_dir_all(stub.parent().unwrap()).unwrap();
  std::os::unix::fs::symlink(&layout.shim, &stub).unwrap();

  let output = Command::new(&stub).output().unwrap();

  let out = stdout(&output);
  assert!(out.contains(&format!("NEEDY_STUB_EXE={}\n", layout.shim.display())), "{out}");
  assert!(out.contains(&format!("NEEDY_STUB_EXECFN={}\n", stub.display())), "{out}");
}

#[test]
fn a_shim_replaces_stub_variables_it_inherited() {
  let layout = layout();
  layout.inject();
  let tool = layout.shim_at("usr/bin/ls");

  let output = Command::new(&tool).env("NEEDY_STUB_EXE", "/stale").env("NEEDY_STUB_EXECFN", "/stale").output().unwrap();

  let out = stdout(&output);
  assert_eq!(out.matches("NEEDY_STUB_EXE=").count(), 1, "{out}");
  assert_eq!(out.matches("NEEDY_STUB_EXECFN=").count(), 1, "{out}");
  assert!(!out.contains("/stale"), "{out}");
}

#[test]
fn a_shim_without_a_guest_needy_says_so_and_exits_127() {
  let layout = layout();
  let tool = layout.shim_at("usr/bin/ls");

  let output = Command::new(&tool).arg0("ls").output().unwrap();

  assert_eq!(output.status.code(), Some(127));
  assert_eq!(
    stderr(&output),
    format!(
      "needy: cannot run ls: {}: No such file or directory (the daemon did not inject the guest needy)\n",
      layout.needy.join("needy").display()
    )
  );
}

#[test]
fn the_boot_stub_runs_an_injected_guest_needy_as_init() {
  let layout = layout();
  layout.inject();

  let output = Command::new(&layout.boot).env("TERM", "linux").output().unwrap();

  assert!(output.status.success(), "{}", stderr(&output));
  assert_eq!(stdout(&output), format!("argv[0]={}\nargv[1]=init\n", layout.needy.join("needy").display()));
}

#[test]
fn the_boot_stub_without_a_guest_needy_says_so_and_exits_127() {
  let layout = layout();

  let output = Command::new(&layout.boot).output().unwrap();

  assert_eq!(output.status.code(), Some(127));
  assert_eq!(
    stderr(&output),
    format!("needy: cannot find the guest needy {}: No such file or directory\n", layout.needy.join("needy").display())
  );
}

#[test]
fn the_boot_stub_mounts_the_guest_disk_over_the_placeholder() {
  // The base's empty `/.needy/needy` means a VM: the stub mounts the guest
  // disk, which fails here (no such device, and no privilege to mount).
  let layout = layout();
  let placeholder = layout.needy.join("needy");
  needy_common::testing::write_executable(&placeholder, "").unwrap();

  let output = Command::new(&layout.boot).output().unwrap();

  assert_eq!(output.status.code(), Some(127));
  let err = stderr(&output);
  let expected = format!(
    "needy: the guest needy is not injected ({} is the base image's empty placeholder), and cannot mount the guest disk /nonexistent/vdb: ",
    placeholder.display()
  );
  assert!(err.starts_with(&expected), "{err}");
}

#[test]
fn a_shim_over_the_empty_placeholder_says_the_guest_needy_is_not_injected() {
  // A container started without the bind mount still has the base image's
  // empty `/.needy/needy`, which `execve` refuses as no executable format;
  // this shim has no real program to fall back to.
  let layout = layout();
  let placeholder = layout.needy.join("needy");
  needy_common::testing::write_executable(&placeholder, "").unwrap();
  let tool = layout.shim_at("usr/bin/true");

  let output = Command::new(&tool).arg0("true").output().unwrap();

  assert_eq!(output.status.code(), Some(127));
  assert_eq!(
    stderr(&output),
    format!("needy: cannot run true: {}: Exec format error (the daemon did not inject the guest needy)\n", placeholder.display())
  );
}

impl Layout {
  /// The base image's empty `/.needy/needy`: no guest needy is injected.
  fn placeholder(&self) -> PathBuf {
    let placeholder = self.needy.join("needy");
    needy_common::testing::write_executable(&placeholder, "").unwrap();
    placeholder
  }

  /// The real program the shim stage keeps for the shim at `tool`: one that
  /// prints its argv and any stub variables, as the stand-in guest does.
  fn real_of(&self, tool: &Path) {
    let real = self.needy.join("real").join(tool.strip_prefix("/").unwrap());
    std::fs::create_dir_all(real.parent().unwrap()).unwrap();
    let source = self.needy.join("real.c");
    std::fs::write(&source, FAKE_GUEST).unwrap();
    compile(&source, &real, &self.needy);
  }
}

// ROOT CAUSE:
//
// If an image `FROM` the base ran a `RUN` step, it failed, because its
// `/bin/sh` (and every program on PATH) is a shim, and a shim ran nothing
// without the guest needy, which only the daemon injects.
//
// Before the fix, the shim printed `the daemon did not inject the guest
// needy` and exited 127.
// The fix keeps routing to the injected guest, and over the empty
// placeholder runs the shim's real program, unrouted.
#[test]
fn a_shim_over_the_empty_placeholder_runs_its_real_program_unrouted() {
  let layout = layout();
  layout.placeholder();
  let tool = layout.shim_at("usr/bin/python3");
  layout.real_of(&tool);

  let output = Command::new(&tool).arg0("python3").args(["-c", "pass"]).env("NEEDY_STUB_EXE", "/inherited").output().unwrap();

  assert!(output.status.success(), "{}", stderr(&output));
  // The caller's argv and environment, without the variables a stub adds.
  assert_eq!(stdout(&output), "argv[0]=python3\nargv[1]=-c\nargv[2]=pass\nNEEDY_STUB_EXE=/inherited\n");
}

#[test]
fn the_proxy_over_the_empty_placeholder_stands_for_no_program_and_fails() {
  let layout = layout();
  let placeholder = layout.placeholder();

  let output = Command::new(&layout.shim).arg0("proxy").output().unwrap();

  assert_eq!(output.status.code(), Some(127));
  assert_eq!(
    stderr(&output),
    format!("needy: cannot run proxy: {}: Exec format error (the daemon did not inject the guest needy)\n", placeholder.display())
  );
}
