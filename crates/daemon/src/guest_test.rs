use needy_api_server::control::{Guest, Runs};

use crate::testing::{self, new_run};

const NO_MOUNT: &str = r#"
actor: { driver: linux-firecracker }
user: { name: agent, uid: 1000, gid: 1000 }
executors:
  here: { driver: native }
"#;

const WORKSPACE: &str = r#"
actor:
  driver: linux-firecracker
  workspace: { path: ~/project, from: "executor:here:/srv/ws/", writable: true }
user: { name: agent, uid: 1000, gid: 1000 }
executors:
  here: { driver: native }
"#;

async fn cwd(profile: &str) -> String {
  let started = testing::daemon().await;
  let created = Runs::create(&started.daemon, new_run(profile, started.dir.path())).await.unwrap();
  Guest::config(&started.daemon, &created.id).await.unwrap().cwd
}

// ROOT CAUSE:
//
// If a profile had no workspace mount and no `actor.cwd`, the driving
// program could not start, because its default working directory was
// `~/workspace` and only a workspace mount creates it; init mounts a fresh
// tmpfs on the home directory, which hides the image's own.
//
// Before the fix, the default was `~/workspace` whatever the mounts.
// The fix makes it the home directory; since `actor.workspace` (2026-11-12)
// the workspace's path is the only other working directory.
#[tokio::test]
async fn an_actor_without_a_workspace_starts_in_its_home_directory() {
  assert_eq!(cwd(NO_MOUNT).await, "/home/agent");
}

#[tokio::test]
async fn an_actor_starts_in_its_workspace_which_is_mounted_from_the_executor() {
  let started = testing::daemon().await;
  let created = Runs::create(&started.daemon, new_run(WORKSPACE, started.dir.path())).await.unwrap();
  let config = Guest::config(&started.daemon, &created.id).await.unwrap();

  assert_eq!(config.cwd, "/home/agent/project");
  let targets: Vec<_> = config.mounts.iter().map(|m| (m.target.as_str(), m.source.as_str(), m.writable)).collect();
  assert_eq!(targets, [("/home/agent/project", "/srv/ws", true)]);
}

#[tokio::test]
async fn a_fresh_workspace_is_the_working_directory_without_a_mount() {
  let profile =
    NO_MOUNT.replace("actor: { driver: linux-firecracker }", "actor: { driver: linux-firecracker, workspace: { path: ~/scratch } }");
  assert_eq!(cwd(&profile).await, "/home/agent/scratch");
}

// `needy run create -- ARGS` appends ARGS to the profile's driving program
// (owner decision, 2026-11-12), so `opencode --continue` needs no profile of
// its own.
#[tokio::test]
async fn run_arguments_are_appended_to_the_profiles_driving_program() {
  let started = testing::daemon().await;
  let profile = NO_MOUNT.replace("actor: { driver: linux-firecracker }", "actor: { driver: linux-firecracker, command: [opencode] }");
  let mut request = new_run(&profile, started.dir.path());
  request.actor_args = vec!["--continue".into(), "a b".into()];
  let created = Runs::create(&started.daemon, request).await.unwrap();

  let config = Guest::config(&started.daemon, &created.id).await.unwrap();

  assert_eq!(config.command, ["opencode", "--continue", "a b"]);
}
