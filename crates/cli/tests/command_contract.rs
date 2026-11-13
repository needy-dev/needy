//! The public command grammar, exercised through the installed test binary.

use std::process::Command;

#[test]
fn help_lists_only_the_canonical_command_groups() {
  let output = Command::new(env!("CARGO_BIN_EXE_needy")).arg("--help").output().unwrap();
  assert!(output.status.success());
  let help = String::from_utf8(output.stdout).unwrap();
  let commands = help.split("Commands:\n").nth(1).unwrap().split("Options:").next().unwrap();
  let names: Vec<_> = commands.lines().filter_map(|line| line.split_whitespace().next()).collect();
  assert_eq!(
    names,
    [
      "config", "daemon", "client", "runner", "run", "proc", "fs", "image", "help"
    ]
  );
}

#[test]
fn every_catalogued_leaf_exposes_usage_without_connecting() {
  let leaves = [
    "config ls",
    "config show",
    "config get",
    "config set",
    "config unset",
    "config paths",
    "config use",
    "config context ls",
    "config context show",
    "config context pair",
    "config context set",
    "config context check",
    "config context rm",
    "config profile ls",
    "config profile show",
    "config profile check",
    "daemon serve",
    "daemon executor",
    "daemon service install",
    "daemon service uninstall",
    "daemon service start",
    "daemon service stop",
    "daemon service restart",
    "daemon service status",
    "daemon service logs",
    "client invite",
    "client ls",
    "client revoke",
    "runner invite",
    "runner join",
    "runner serve",
    "runner ls",
    "runner rm",
    "runner rotate",
    "runner token ls",
    "runner token revoke",
    "run create",
    "run ls",
    "run stop",
    "run attach",
    "run logs",
    "run exec",
    "run report",
    "run chaos add",
    "run chaos ls",
    "run chaos rm",
    "proc exec",
    "proc ls",
    "proc attach",
    "proc kill",
    "fs cp",
    "image pull",
    "image ls",
    "image build",
    "image rm",
  ];
  for leaf in leaves {
    let output = Command::new(env!("CARGO_BIN_EXE_needy")).args(leaf.split_whitespace()).arg("--help").output().unwrap();
    assert!(output.status.success(), "{leaf}: {}", String::from_utf8_lossy(&output.stderr));
    assert!(String::from_utf8_lossy(&output.stdout).contains("Usage:"), "{leaf}");
  }
}
