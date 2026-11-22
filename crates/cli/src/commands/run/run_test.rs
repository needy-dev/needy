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
  assert_eq!(cells[3], "-", "an ended run's actor state is not kept");
  assert_eq!(cells[5], "mac=runner (lost), build=linux-container (released)");
  assert_eq!(cells[6], "the control plane restarted; the actor was lost");
}

#[test]
fn run_ls_shows_where_the_actor_stands() {
  let wire = [
    (v1::ActorState::Unspecified, "-"),
    (v1::ActorState::Starting, "starting"),
    (v1::ActorState::Configured, "configured"),
    (v1::ActorState::Ready, "ready"),
    (v1::ActorState::Exited, "exited"),
  ];

  for (value, shown) in wire {
    let run = Run {
      run_id: "r1".into(),
      state: v1::RunState::Running as i32,
      actor_state: value as i32,
      ..Default::default()
    };
    assert_eq!(RunRow::try_from(run).unwrap().cells()[3], shown, "{value:?}");
  }
  let newer = Run {
    run_id: "r1".into(),
    state: v1::RunState::Running as i32,
    actor_state: 42,
    ..Default::default()
  };
  assert_eq!(format!("{:#}", RunRow::try_from(newer).err().unwrap()), "run r1: unknown actor state 42");
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

#[test]
fn create_accepts_local_file_and_temporary_overrides() {
  use clap::Parser as _;
  let line = Line::try_parse_from([
    "run",
    "create",
    "-f",
    "profile.yaml",
    "--image",
    "example.com/actor:trial",
    "--workspace",
    "/work",
    "--env",
    "MODE=trial",
    "--entry-command",
    "sh -l",
    "--",
    "extra",
  ])
  .unwrap();
  let Command::Create { profile, args, .. } = line.command else {
    panic!("not create")
  };
  assert_eq!(profile, PathBuf::from("profile.yaml"));
  assert_eq!(args, ["extra"]);
}

#[test]
fn run_create_accepts_short_e_env_flag() {
  use clap::Parser as _;
  let line = Line::try_parse_from(["run", "create", "-f", "profile.yaml", "-e", "FOO=bar"]).unwrap();
  let Command::Create { overrides, .. } = line.command else {
    panic!("not create");
  };
  assert_eq!(overrides.env, vec![("FOO".into(), "bar".into())]);

  let err = Line::try_parse_from(["run", "create", "-f", "profile.yaml", "-e", "INVALID"]).err().expect("must reject");
  assert!(err.to_string().contains("KEY=VALUE"), "{err}");
}

fn workspace(executor: &str, source: &str, writable: bool) -> v1::RunWorkspace {
  v1::RunWorkspace {
    path: "/home/agent/workspace".into(),
    executor: executor.into(),
    source: source.into(),
    writable,
  }
}

fn profile(executors: &str) -> needy_config::profile::Profile {
  let yaml = format!("actor: {{ driver: linux-firecracker }}\nexecutors:\n{executors}");
  needy_config::profile::load(&needy_config::Source::text(yaml, "/base")).unwrap()
}

#[test]
fn run_create_names_the_machine_and_folder_of_its_workspace() {
  let client = profile("  here: { driver: native, runner: { client: true } }\n");
  let control = profile("  here: { driver: native }\n");
  let runner = profile("  here: { driver: native, runner: mac }\n");

  assert_eq!(
    workspace_line(&workspace("here", "/Users/user/project", true), &client),
    "workspace /home/agent/workspace: live from /Users/user/project on this machine (executor here)"
  );
  assert_eq!(
    workspace_line(&workspace("here", "/home/user/project", false), &control),
    "workspace /home/agent/workspace: live from /home/user/project on the control host (executor here), read-only"
  );
  assert_eq!(
    workspace_line(&workspace("here", "/srv/ws", true), &runner),
    "workspace /home/agent/workspace: live from /srv/ws on runner mac (executor here)"
  );
  assert_eq!(
    workspace_line(&workspace("", "/home/user/seed", false), &control),
    "workspace /home/agent/workspace: copied from /home/user/seed on the control host, read-only"
  );
  assert_eq!(workspace_line(&workspace("", "", true), &control), "workspace /home/agent/workspace: a fresh directory in the actor");
}
