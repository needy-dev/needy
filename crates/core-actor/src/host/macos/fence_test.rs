use std::path::{Path, PathBuf};

use super::*;

fn fence() -> Fence {
  Fence {
    tag: "needy-r1".into(),
    shims: PathBuf::from("/Users/user/.local/state/needy/runs/r1/actor/shims"),
    shim: PathBuf::from("/Users/user/bin/needy"),
    gateway_port: 43128,
    sockets: vec![
      PathBuf::from("/Users/user/.local/state/needy/runs/r1/channels/guest.sock"),
      PathBuf::from("/Users/user/.local/state/needy/runs/r1/channels/kept.sock"),
    ],
    writable: vec![
      PathBuf::from("/Users/user/.local/state/needy/runs/r1/actor/home"),
      PathBuf::from("/Users/user/needy-ws/demo"),
    ],
    hidden: vec![PathBuf::from("/Users/user/.ssh")],
  }
}

#[test]
fn every_exec_but_the_shims_the_shim_and_the_kept_program_is_denied_with_the_tag() {
  let profile = fence().render();
  assert!(profile.contains("(deny process-exec* (with message \"needy-r1\"))"), "{profile}");
  assert!(
    profile.contains(
      "(allow process-exec* (subpath \"/Users/user/.local/state/needy/runs/r1/actor/shims\") (literal \"/Users/user/bin/needy\") (literal (param \"KEPT\")) (literal (param \"KEPT_2\")) (literal (param \"KEPT_3\")) (subpath (param \"TOOLS\")))"
    ),
    "{profile}"
  );
  // The allow comes after the deny: Seatbelt lets the later rule win.
  assert!(profile.find("(deny process-exec*").unwrap() < profile.find("(allow process-exec*").unwrap());
}

#[test]
fn the_network_is_the_gateway_port_and_the_actors_sockets_only() {
  let profile = fence().render();
  assert!(profile.contains("(deny network* (with message \"needy-r1\"))"), "{profile}");
  assert!(profile.contains("(allow network-outbound (remote ip \"localhost:43128\"))"), "{profile}");
  // One rule per socket: a rule with two paths allows only the first.
  for socket in ["guest", "kept"] {
    let rule = format!(
      "(allow network-outbound (remote unix-socket (path-literal \"/Users/user/.local/state/needy/runs/r1/channels/{socket}.sock\")))"
    );
    assert!(profile.contains(&rule), "{rule} missing: {profile}");
  }
  assert!(!profile.contains("network-bind"), "a fenced program listens nowhere: {profile}");
}

#[test]
fn writes_outside_the_actors_directories_and_devices_are_denied() {
  let profile = fence().render();
  assert!(
    profile.contains(
      "(deny file-write* (with message \"needy-r1\") (require-not (require-any (subpath \"/Users/user/.local/state/needy/runs/r1/actor/home\") (subpath \"/Users/user/needy-ws/demo\") (literal \"/dev/null\") (literal \"/dev/zero\") (literal \"/dev/tty\") (literal \"/dev/dtracehelper\") (regex #\"^/dev/ttys[0-9]+$\") (regex #\"^/private/var/folders/[^/]+/[^/]+/T/xcrun_db\"))))"
    ),
    "{profile}"
  );
}

#[test]
fn the_users_secrets_are_unreadable() {
  let profile = fence().render();
  assert!(profile.contains("(deny file-read* (with message \"needy-r1\") (subpath \"/Users/user/.ssh\"))"), "{profile}");
}

#[test]
fn programs_that_launch_outside_the_sandbox_are_denied() {
  let profile = fence().render();
  for rule in [
    "(deny lsopen",
    "(deny appleevent-send",
    "(deny job-creation",
    "(global-name \"com.apple.coreservices.launchservicesd\")",
  ] {
    assert!(profile.contains(rule), "{rule} missing: {profile}");
  }
}

#[test]
fn quotes_and_backslashes_in_paths_are_escaped() {
  let mut fence = fence();
  fence.writable = vec![PathBuf::from("/Users/user/a \"b\"\\c")];
  let profile = fence.render();
  assert!(profile.contains("(subpath \"/Users/user/a \\\"b\\\"\\\\c\")"), "{profile}");
}

#[test]
fn a_kept_program_gets_every_parameter() {
  let args = arguments(Path::new("/run/fence.sb"), &[PathBuf::from("/usr/bin/uname")], None);
  assert_eq!(
    args,
    [
      "-f",
      "/run/fence.sb",
      "-D",
      "KEPT=/usr/bin/uname",
      "-D",
      "KEPT_2=/usr/bin/uname",
      "-D",
      "KEPT_3=/usr/bin/uname",
      "-D",
      "TOOLS=/var/empty",
      "/usr/bin/uname"
    ]
  );
}

#[test]
fn macos_sh_may_run_bash_which_it_reexecutes() {
  assert_eq!(allowed(Path::new("/bin/sh")), [PathBuf::from("/bin/sh"), PathBuf::from("/bin/bash")]);
}

// Seatbelt checks a script and its `#!` interpreter both: with only the
// script allowed, `sandbox-exec` fails with EPERM (macOS 27.2).
#[test]
fn a_script_may_run_its_interpreter() {
  let dir = tempfile::tempdir().unwrap();
  let script = dir.path().join("tool");
  std::fs::write(&script, "#!/bin/sh -e\necho hi\n").unwrap();
  assert_eq!(
    allowed(&script),
    [
      script.clone(),
      PathBuf::from("/bin/sh"),
      PathBuf::from("/bin/bash")
    ]
  );
  let env = dir.path().join("npx");
  std::fs::write(&env, "#!/usr/bin/env node\n").unwrap();
  assert_eq!(allowed(&env), [env.clone(), PathBuf::from("/usr/bin/env")]);
  let args = arguments(Path::new("/run/fence.sb"), &allowed(&env), None);
  assert_eq!(args[5], "KEPT_2=/usr/bin/env");
  assert_eq!(args.last().map(String::as_str), Some(env.to_str().unwrap()));
}

#[test]
fn a_developer_tools_trampoline_may_run_the_developer_directory() {
  let developer = tempfile::tempdir().unwrap();
  std::fs::create_dir_all(developer.path().join("usr/bin")).unwrap();
  std::fs::write(developer.path().join("usr/bin/python3"), "").unwrap();
  assert_eq!(tools(Path::new("/usr/bin/python3"), Some(developer.path())).as_deref(), Some(developer.path()));
  assert_eq!(tools(Path::new("/usr/bin/uname"), Some(developer.path())), None);
  assert_eq!(tools(Path::new("/opt/homebrew/bin/python3"), Some(developer.path())), None);
  assert_eq!(tools(Path::new("/usr/bin/python3"), None), None);
  let args = arguments(Path::new("/run/fence.sb"), &[PathBuf::from("/usr/bin/python3")], Some(developer.path()));
  assert_eq!(args[9], format!("TOOLS={}", developer.path().display()));
}
