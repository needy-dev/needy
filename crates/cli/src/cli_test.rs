//! Command lines that other crates and images write for this CLI, parsed by
//! the CLI's own definition so a change to its shape breaks them here.

use std::path::{Path, PathBuf};

use clap::CommandFactory;
use needy_core_executor::driver::Spec;
use needy_core_executor::driver::linux_container::{self, Options};

use super::Cli;

/// Asserts that `argv` is `needy daemon executor` with a Unix listener the
/// executor accepts, and returns its `--name`.
fn executor_name(argv: &[String]) -> String {
  let matches = Cli::command().try_get_matches_from(argv).unwrap_or_else(|e| panic!("{argv:?} is not a needy command line: {e}"));
  let (command, daemon) = matches.subcommand().unwrap();
  assert_eq!(command, "daemon", "{argv:?}");
  let (command, executor) = daemon.subcommand().unwrap();
  assert_eq!(command, "executor", "{argv:?}");
  let listen = executor.get_one::<String>("listen").unwrap();
  assert!(listen.starts_with("unix:"), "{argv:?}");
  // The command runs in a Linux container, so its Unix path is checked only
  // where `Path` has Unix semantics (on Windows `/run/...` is not absolute).
  #[cfg(unix)]
  needy_config::global::validate_address(listen, true).unwrap();
  executor.get_one::<String>("name").unwrap().clone()
}

// ROOT CAUSE:
//
// If a profile used a `linux-container` executor, the container exited 2
// (`unexpected argument '--mode'`) and `run create` could not reach its
// socket, because the driver still wrote `needy daemon --mode executor` after
// the CLI replaced `--mode` with the `daemon executor` subcommand.
//
// Before the fix, the driver's own test asserted the same stale literal, so
// nothing parsed the command line with the CLI that runs it.
// The fix keeps the driver's command line parseable by `Cli` as `daemon
// executor`.
#[test]
fn the_linux_container_executor_command_is_a_daemon_executor_command_line() {
  let spec = Spec {
    name: "build".into(),
    key: "build@s1".into(),
    run: "s1".into(),
    config: Default::default(),
    base: PathBuf::from("/home/user/profile"),
    user: Default::default(),
    hostname: None,
  };
  let options = Options {
    image: "needy-executor:latest".into(),
    state_dir: PathBuf::from("/home/user/state"),
    allow: Vec::new(),
  };
  let container = linux_container::container(&spec, &options).unwrap();

  assert_eq!(executor_name(container.body.cmd.as_deref().unwrap()), "build");
}

// The `executor-base` image's `CMD` had the same stale `--mode executor`; a
// container started from the image alone exited 2.
#[test]
fn the_executor_image_command_is_a_daemon_executor_command_line() {
  let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../images/Dockerfile");
  let dockerfile = std::fs::read_to_string(&path).unwrap();
  let stage = dockerfile
    .split("\nFROM ")
    .find(|stage| stage.lines().next().is_some_and(|from| from.ends_with(" AS executor-base")))
    .expect("the Dockerfile has an executor-base stage");
  let cmd = stage.lines().find_map(|line| line.strip_prefix("CMD ")).expect("executor-base has a CMD");
  let argv: Vec<String> = serde_json::from_str(cmd).expect("CMD is in exec form");

  assert_eq!(executor_name(&argv), "executor");
}
