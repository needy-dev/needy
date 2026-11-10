use needy_api_server::control::{Guest, Runs};

use crate::testing::{self, new_run};

const NO_MOUNT: &str = r#"
user: { name: agent, uid: 1000, gid: 1000 }
executors:
  here: { provider: native }
"#;

const WORKSPACE_MOUNT: &str = r#"
user: { name: agent, uid: 1000, gid: 1000 }
executors:
  here: { provider: native }
files:
  workspace: { mode: mount, from: "executor:here:/srv/ws/", to: "actor:~/workspace/", writable: true }
run_files: [workspace]
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
// The fix makes it the home directory when no mount provides `~/workspace`.
#[tokio::test]
async fn an_actor_without_a_workspace_mount_starts_in_its_home_directory() {
  assert_eq!(cwd(NO_MOUNT).await, "/home/agent");
}

#[tokio::test]
async fn an_actor_with_a_workspace_mount_starts_in_the_workspace() {
  assert_eq!(cwd(WORKSPACE_MOUNT).await, "/home/agent/workspace");
}

#[tokio::test]
async fn an_actor_cwd_wins_over_the_default() {
  let profile = format!("{NO_MOUNT}actor: {{ cwd: /tmp }}\n");
  assert_eq!(cwd(&profile).await, "/tmp");
}
