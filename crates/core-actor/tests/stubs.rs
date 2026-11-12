//! The base image's stubs (`images/stub/`), built with this machine's C
//! compiler against a directory of the test's own (`-DNEEDY_DIR`), and run
//! with a stand-in guest needy that prints what it was given. They are the
//! base ABI's hand-off, so these tests pin it: see
//! `docs/ai/references/architecture/2026-11-12-actor-images-design.md`.
//!
//! Linux only: the stubs read `/proc/self/exe` and the auxiliary vector.
#![cfg(target_os = "linux")]

use std::os::unix::fs::PermissionsExt;
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
  std::fs::write(&placeholder, "").unwrap();
  std::fs::set_permissions(&placeholder, std::fs::Permissions::from_mode(0o755)).unwrap();

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
  // empty `/.needy/needy`, which `execve` refuses as no executable format.
  let layout = layout();
  let placeholder = layout.needy.join("needy");
  std::fs::write(&placeholder, "").unwrap();
  std::fs::set_permissions(&placeholder, std::fs::Permissions::from_mode(0o755)).unwrap();
  let tool = layout.shim_at("usr/bin/true");

  let output = Command::new(&tool).arg0("true").output().unwrap();

  assert_eq!(output.status.code(), Some(127));
  assert_eq!(
    stderr(&output),
    format!("needy: cannot run true: {}: Exec format error (the daemon did not inject the guest needy)\n", placeholder.display())
  );
}
