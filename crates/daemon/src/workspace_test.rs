use std::path::Path;

use needy_api_server::control::{ControlError, Guest, NewRun, Runs};
use needy_config::profile::{ExecutorDriver, Side};
use needy_core::intercept::exec::ExecContext;

use crate::testing::{self, new_run};

const NATIVE: &str = r#"
actor: { driver: linux-firecracker }
user: { name: agent, uid: 1000, gid: 1000 }
executors:
  here: { driver: native }
commands:
  - { match: "python3 **", to: here }
"#;

/// A create from `folder` by a CLI on the control host.
fn from_here(profile: &str, dir: &Path, folder: &Path) -> NewRun {
  let mut request = new_run(profile, dir);
  request.client_dir = folder.display().to_string();
  request.on_control_host = true;
  request
}

fn failure(result: Result<needy_api_server::control::RunInfo, ControlError>) -> String {
  match result {
    Err(ControlError::Failed(message)) => message,
    other => panic!("the create must fail, got {other:?}"),
  }
}

#[tokio::test]
async fn a_run_without_a_workspace_works_live_in_the_folder_it_was_created_in() {
  let started = testing::daemon().await;
  let folder = started.dir.path().join("project");
  std::fs::create_dir(&folder).unwrap();

  let created = Runs::create(&started.daemon, from_here(NATIVE, started.dir.path(), &folder)).await.unwrap();

  let config = Guest::config(&started.daemon, &created.id).await.unwrap();
  assert_eq!(config.cwd, "/home/agent/workspace");
  let mounts: Vec<_> = config.mounts.iter().map(|m| (m.target.as_str(), m.source.as_str(), m.writable)).collect();
  assert_eq!(mounts, [("/home/agent/workspace", folder.to_str().unwrap(), true)]);
  // Routed commands run on the same files, through the derived path map.
  let ctx = ExecContext {
    argv: vec!["python3".into(), "/home/agent/workspace/a.py".into()],
    cwd: "/home/agent/workspace/sub".into(),
    ..ExecContext::default()
  };
  let route = Runs::decide(&started.daemon, &created.id, ctx).await.unwrap();
  assert_eq!(
    route.argv,
    [
      "python3".to_string(),
      folder.join("a.py").display().to_string()
    ]
  );
  assert_eq!(route.cwd, folder.join("sub").display().to_string());
  // The run names its workspace.
  let workspace = created.workspace.expect("the run names its workspace");
  assert_eq!(workspace.path, "/home/agent/workspace");
  assert_eq!(workspace.executor, "here");
  assert_eq!(workspace.source, folder.display().to_string());
  assert!(workspace.writable);
}

#[tokio::test]
async fn a_folder_on_another_machine_than_the_executors_fails_the_create_and_asks_for_workspace_from() {
  let started = testing::daemon().await;
  let mut request = new_run(NATIVE, started.dir.path());
  request.client_dir = "/home/user/project".into();
  request.on_control_host = false;

  let message = failure(Runs::create(&started.daemon, request).await);

  assert!(message.contains("/home/user/project"), "{message}");
  assert!(message.contains("workspace.from"), "{message}");
  assert!(message.contains("executor here"), "{message}");
}

#[tokio::test]
async fn an_executor_on_a_runner_cannot_reach_the_folder() {
  let started = testing::daemon().await;
  let profile = NATIVE.replace("here: { driver: native }", "here: { driver: native, runner: elsewhere }");

  let message = failure(Runs::create(&started.daemon, from_here(&profile, started.dir.path(), Path::new("/home/user/project"))).await);

  assert!(message.contains("runner elsewhere"), "{message}");
  assert!(message.contains("workspace.from"), "{message}");
}

// NOTICE: without an executor the actor runs every command itself and
// nothing can pass the folder through, so the actor starts in its home as
// before; see `default` in `workspace.rs`.
#[tokio::test]
async fn a_profile_without_executors_keeps_the_actors_home() {
  let started = testing::daemon().await;
  let profile = "actor: { driver: linux-firecracker }\nuser: { name: agent, uid: 1000, gid: 1000 }\n";

  let created = Runs::create(&started.daemon, from_here(profile, started.dir.path(), started.dir.path())).await.unwrap();

  assert_eq!(Guest::config(&started.daemon, &created.id).await.unwrap().cwd, "/home/agent");
  assert_eq!(created.workspace, None);
}

#[tokio::test]
async fn a_written_workspace_wins_over_the_folder() {
  let started = testing::daemon().await;
  let profile = NATIVE.replace(
    "actor: { driver: linux-firecracker }",
    "actor: { driver: linux-firecracker, workspace: { path: ~/project, from: \"executor:here:/srv/ws\" } }",
  );

  let created = Runs::create(&started.daemon, from_here(&profile, started.dir.path(), started.dir.path())).await.unwrap();

  let config = Guest::config(&started.daemon, &created.id).await.unwrap();
  assert_eq!(config.cwd, "/home/agent/project");
  assert_eq!(config.mounts.iter().map(|m| m.source.as_str()).collect::<Vec<_>>(), ["/srv/ws"]);
  let workspace = created.workspace.unwrap();
  assert_eq!((workspace.source.as_str(), workspace.writable), ("/srv/ws", false));
}

#[tokio::test]
async fn a_create_that_sends_no_folder_keeps_the_actors_home() {
  let started = testing::daemon().await;

  let created = Runs::create(&started.daemon, new_run(NATIVE, started.dir.path())).await.unwrap();

  assert_eq!(Guest::config(&started.daemon, &created.id).await.unwrap().cwd, "/home/agent");
}

#[tokio::test]
async fn several_executors_need_a_default_executor_to_pick_the_workspaces() {
  let started = testing::daemon().await;
  let several = NATIVE.replace("here: { driver: native }", "here: { driver: native }\n  there: { driver: native }");

  let message = failure(Runs::create(&started.daemon, from_here(&several, started.dir.path(), started.dir.path())).await);
  assert!(message.contains("default_executor"), "{message}");

  let chosen = format!("{several}default_executor: here\n");
  let created = Runs::create(&started.daemon, from_here(&chosen, started.dir.path(), started.dir.path())).await.unwrap();
  assert_eq!(created.workspace.unwrap().executor, "here");
}

#[tokio::test]
async fn a_container_executor_that_binds_its_own_workspace_cannot_bind_the_folder() {
  let started = testing::daemon().await;
  let profile = NATIVE.replace("here: { driver: native }", "here: { driver: linux-container, workspace: /srv/ws }");

  let message = failure(Runs::create(&started.daemon, from_here(&profile, started.dir.path(), Path::new("/home/user/project"))).await);

  assert!(message.contains("/srv/ws"), "{message}");
  assert!(message.contains("workspace.from"), "{message}");
}

// A `linux-container` executor binds the folder as its workspace, which its
// container sees at `~/workspace`; creating one needs an engine, so this
// checks the derived profile.
#[test]
fn a_container_executor_on_the_control_host_binds_the_folder() {
  let source = needy_config::Source::text(NATIVE.replace("{ driver: native }", "{ driver: linux-container }"), "/base");
  let mut profile = needy_config::profile::load(&source).unwrap();

  super::default(&mut profile, "/home/user/project", true, &source).unwrap();

  let executor = &profile.executors["here"];
  assert_eq!(executor.driver, ExecutorDriver::LinuxContainer);
  assert_eq!(executor.workspace.as_deref(), Some("/home/user/project"));
  assert_eq!(executor.path_map["/home/agent/workspace"], "~/workspace");
  let mount = &profile.files.mappings[needy_config::profile::WORKSPACE];
  assert_eq!(
    mount.from,
    Side::Executor {
      name: "here".into(),
      path: "~/workspace".into()
    }
  );
  assert!(mount.writable);
}

// The client's own executor runs on the machine the create came from,
// wherever the control host is.
#[tokio::test]
async fn a_client_executor_reaches_the_folder_from_another_machine() {
  use std::time::Duration;

  use tokio_util::task::AbortOnDropHandle;

  let started = testing::daemon().await;
  let served = testing::serve(started, None).await;
  let daemon = served.daemon();
  let folder = served.dir().join("project");
  std::fs::create_dir(&folder).unwrap();
  let reserved = Runs::prepare_client_execution(daemon, Default::default()).await.unwrap();
  let holder = reserved.holder.clone();
  let _helper = AbortOnDropHandle::new(tokio::spawn(crate::runner::serve_client_execution(crate::runner::ClientExecutionConfig {
    control: Some(served.endpoint.clone()),
    command: None,
    holder: holder.clone(),
    capability: reserved.capability.clone(),
    run: reserved.run,
    directory: served.dir().join("client-holder"),
  })));
  daemon.links.connected(&holder, Duration::from_secs(5)).await.expect("client holder connected");
  let profile = NATIVE.replace("here: { driver: native }", "here: { driver: native, runner: { client: true } }");
  let mut request = new_run(&profile, served.dir());
  request.client_execution = reserved.capability;
  request.client_dir = folder.display().to_string();
  request.on_control_host = false;

  let created = Runs::create(daemon, request).await.unwrap();

  let config = Guest::config(daemon, &created.id).await.unwrap();
  let mounts: Vec<_> = config.mounts.iter().map(|m| (m.target.as_str(), m.source.as_str())).collect();
  assert_eq!(mounts, [("/home/agent/workspace", folder.to_str().unwrap())]);
  Runs::stop(daemon, &created.id, needy_core::types::Principal::Operator, needy_api_server::control::StopMode::Now).await.unwrap();
}
