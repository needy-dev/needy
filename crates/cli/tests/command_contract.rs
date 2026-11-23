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
    "image edit",
    "image rollback",
    "image log",
    "image export",
    "image rm",
  ];
  for leaf in leaves {
    let output = Command::new(env!("CARGO_BIN_EXE_needy")).args(leaf.split_whitespace()).arg("--help").output().unwrap();
    assert!(output.status.success(), "{leaf}: {}", String::from_utf8_lossy(&output.stderr));
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(help.contains("Usage:"), "{leaf}: missing Usage");
    let before_usage = help.split("Usage:").next().unwrap_or("").trim();
    assert!(!before_usage.is_empty(), "{leaf}: missing non-empty about description in --help:\n{help}");
  }
}

#[test]
fn image_export_uses_file_instead_of_output_format_flag() {
  let output = Command::new(env!("CARGO_BIN_EXE_needy")).args(["image", "export", "--help"]).output().unwrap();
  assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
  let help = String::from_utf8(output.stdout).unwrap();
  assert!(help.contains("--file <PATH>"), "expected --file <PATH> in image export --help:\n{help}");
  assert!(!help.contains("-o,"), "image export must not define -o:\n{help}");
  assert!(!help.contains("--output"), "image export must not define --output:\n{help}");
}

#[test]
fn local_only_commands_do_not_expose_remote_connection_flags() {
  let local_only = [
    "daemon serve",
    "daemon service status",
    "image pull",
    "image ls",
    "image rm",
    "config ls",
    "config paths",
  ];
  for leaf in local_only {
    let output = Command::new(env!("CARGO_BIN_EXE_needy")).args(leaf.split_whitespace()).arg("--help").output().unwrap();
    assert!(output.status.success(), "{leaf}: {}", String::from_utf8_lossy(&output.stderr));
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(!help.contains("--context"), "{leaf}: local command must not expose --context:\n{help}");
    assert!(!help.contains("--address"), "{leaf}: local command must not expose --address:\n{help}");
    assert!(!help.contains("--credential-file"), "{leaf}: local command must not expose --credential-file:\n{help}");
  }
}

#[test]
fn runner_serve_exposes_preprovisioned_credential_and_activity_hook_flags() {
  let output = Command::new(env!("CARGO_BIN_EXE_needy")).args(["runner", "serve", "--help"]).output().unwrap();
  assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
  let help = String::from_utf8(output.stdout).unwrap();
  assert!(!help.contains("--context"), "runner serve must not expose --context:\n{help}");
  for flag in [
    "--address",
    "--name",
    "--credential-env",
    "--credential-file",
    "--activity-hook",
    "--actor-image",
  ] {
    assert!(help.contains(flag), "runner serve --help must expose {flag}:\n{help}");
  }
}

#[test]
fn root_help_exposes_quiet_and_hides_remote_connection_flags() {
  let output = Command::new(env!("CARGO_BIN_EXE_needy")).arg("--help").output().unwrap();
  assert!(output.status.success());
  let help = String::from_utf8(output.stdout).unwrap();
  assert!(help.contains("-q, --quiet"), "root --help must expose -q, --quiet:\n{help}");
  assert!(!help.contains("--context"), "root --help must not expose global --context:\n{help}");
  assert!(!help.contains("--address"), "root --help must not expose global --address:\n{help}");
  assert!(!help.contains("--credential-file"), "root --help must not expose global --credential-file:\n{help}");
}
