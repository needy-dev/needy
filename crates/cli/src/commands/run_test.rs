use needy_api_client::proto::v1;
use needy_core::types::RunState;

use super::*;

#[test]
fn every_run_state_is_read_from_the_wire() {
  let wire = [
    (v1::RunState::Creating, RunState::Creating),
    (v1::RunState::Running, RunState::Running),
    (v1::RunState::Stopping, RunState::Stopping),
    (v1::RunState::Stopped, RunState::Stopped),
    (v1::RunState::Finished, RunState::Finished),
    (v1::RunState::Failed, RunState::Failed),
  ];

  for (value, expected) in wire {
    assert_eq!(state(value as i32).unwrap(), expected);
  }
}

#[test]
fn an_unspecified_run_state_is_refused() {
  let error = state(v1::RunState::Unspecified as i32).unwrap_err();

  assert_eq!(error.to_string(), "the run state is unspecified");
}

#[test]
fn an_unknown_run_state_is_refused() {
  let error = state(42).unwrap_err();

  assert_eq!(error.to_string(), "unknown run state 42");
}

#[test]
fn run_ls_marks_a_lost_executor_and_a_released_one() {
  let run = Run {
    run_id: "r1".into(),
    name: "r1".into(),
    group: "r1".into(),
    state: v1::RunState::Failed as i32,
    executors: vec![
      v1::ExecutorRef {
        name: "mac".into(),
        driver: "runner".into(),
        instance: "mac".into(),
        released: true,
        lost: true,
      },
      v1::ExecutorRef {
        name: "build".into(),
        driver: "linux-container".into(),
        instance: "c1".into(),
        released: true,
        lost: false,
      },
    ],
    failure: "the control plane restarted; the actor was lost".into(),
    ..Default::default()
  };

  let row = RunRow::try_from(run).unwrap();

  let cells = row.cells();
  assert_eq!(cells[2], "failed");
  assert_eq!(cells[4], "mac=runner (lost), build=linux-container (released)");
  assert_eq!(cells[5], "the control plane restarted; the actor was lost");
}

#[derive(clap::Parser)]
struct Line {
  #[command(subcommand)]
  command: Command,
}

// `needy run create -it --profile P -- ARGS` hands ARGS, flags included, to
// the driving program (owner decision, 2026-11-12); the control plane
// appends them to the profile's `actor.command`.
#[test]
fn arguments_after_the_separator_reach_the_driving_program() {
  use clap::Parser as _;
  let line = Line::try_parse_from([
    "run",
    "create",
    "-it",
    "--profile",
    "opencode",
    "--",
    "--continue",
    "-m",
    "a b",
  ])
  .unwrap();

  let Command::Create {
    profile,
    args,
    tty,
    interactive,
    ..
  } = line.command
  else {
    panic!("not create");
  };
  assert_eq!(profile, PathBuf::from("opencode"));
  assert_eq!(args, ["--continue", "-m", "a b"]);
  assert!(tty && interactive);
}
