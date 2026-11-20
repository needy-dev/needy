use needy_api_server::control::{Chunks, ControlError, Guest, GuestSession, Runs, UpperLayer};
use needy_core_actor::image::store::{Digest, Snapshot};

use super::UPPER_LAYER;
use crate::run::Boot;
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

// ROOT CAUSE:
//
// If a profile named no recipe hostname, every VM actor had the same
// hostname, because init only sets a recipe's and the kernel keeps its
// built-in default otherwise (`sandbox-vm` on macos-vz, `(none)` on
// Firecracker). A harness that tells writers apart by host, such as Kimi
// Code's session locks (pid and hostname), took another actor's lock for one
// of its own host whose process had died, and took it over.
//
// Before the fix, the guest was told an empty hostname.
// The fix names each actor after its run when its recipe gives no hostname.
#[tokio::test]
async fn actors_without_a_recipe_hostname_get_one_named_after_their_run() {
  let started = testing::daemon().await;
  let first = Runs::create(&started.daemon, new_run(NO_MOUNT, started.dir.path())).await.unwrap();
  let second = Runs::create(&started.daemon, new_run(NO_MOUNT, started.dir.path())).await.unwrap();

  let first_host = Guest::config(&started.daemon, &first.id).await.unwrap().hostname;
  let second_host = Guest::config(&started.daemon, &second.id).await.unwrap().hostname;

  assert_eq!(first_host, format!("needy-{}", first.id));
  assert_eq!(second_host, format!("needy-{}", second.id));
  assert_ne!(first_host, second_host);
}

#[tokio::test]
async fn a_recipe_hostname_is_the_actors_hostname() {
  let started = testing::daemon().await;
  let profile = NO_MOUNT
    .replace("actor: { driver: linux-firecracker }", "actor: { driver: linux-firecracker, recipe: builtin:dell-optiplex-ubuntu-24.04 }");
  let created = Runs::create(&started.daemon, new_run(&profile, started.dir.path())).await.unwrap();

  let hostname = Guest::config(&started.daemon, &created.id).await.unwrap().hostname;

  assert!(hostname.starts_with("optiplex-"), "{hostname}");
}

// A container actor's init gives its process socket to these IDs, so a
// daemon that is not root can connect to its driving program's terminal.
#[tokio::test]
async fn the_guest_is_told_the_daemons_user_and_group() {
  let started = testing::daemon().await;
  let created = Runs::create(&started.daemon, new_run(NO_MOUNT, started.dir.path())).await.unwrap();

  let config = Guest::config(&started.daemon, &created.id).await.unwrap();

  // SAFETY: `geteuid` and `getegid` cannot fail.
  let expected = unsafe { (libc::geteuid(), libc::getegid()) };
  assert_eq!((config.daemon_uid, config.daemon_gid), expected);
}

fn layer(name: &str) -> Digest {
  Digest::of(name.as_bytes())
}

#[tokio::test]
async fn the_guest_is_told_its_snapshots_layers_lowest_first_and_the_images_path() {
  let started = testing::daemon().await;
  let created = Runs::create(&started.daemon, new_run(NO_MOUNT, started.dir.path())).await.unwrap();
  let (root, built) = (layer("root"), layer("built"));
  started.daemon.runs.find(&created.id).unwrap().set_boot(Boot {
    snapshot: Some(Snapshot {
      digest: layer("stack"),
      layers: vec![root, built],
    }),
    layer_disks: Vec::new(),
    path: "/usr/local/bin:/usr/bin:/bin".into(),
    env: Default::default(),
    session: GuestSession::default(),
  });

  let config = Guest::config(&started.daemon, &created.id).await.unwrap();

  assert_eq!(config.layers, [root.to_string(), built.to_string()]);
  assert_eq!(config.image_path, "/usr/local/bin:/usr/bin:/bin");
  assert!(!config.env.contains_key("PATH"), "{:?}", config.env);
  assert_eq!(
    config.session,
    GuestSession {
      upper: UpperLayer::Disposable,
      routing: true,
    }
  );
}

#[tokio::test]
async fn a_run_without_a_snapshot_boots_the_base_alone_as_a_run() {
  let started = testing::daemon().await;
  let created = Runs::create(&started.daemon, new_run(NO_MOUNT, started.dir.path())).await.unwrap();

  let config = Guest::config(&started.daemon, &created.id).await.unwrap();

  assert!(config.layers.is_empty());
  assert_eq!(config.image_path, "");
  assert_eq!(config.session, GuestSession::default());
}

fn parts(parts: Vec<Result<Vec<u8>, ControlError>>) -> Chunks {
  Box::pin(futures_util::stream::iter(parts))
}

#[tokio::test]
async fn a_kept_upper_layer_is_written_to_the_runs_directory_whole() {
  let started = testing::daemon().await;
  let created = Runs::create(&started.daemon, new_run(NO_MOUNT, started.dir.path())).await.unwrap();
  let run = started.daemon.runs.find(&created.id).unwrap();
  run.set_boot(Boot {
    session: GuestSession {
      upper: UpperLayer::Kept,
      routing: false,
    },
    ..Boot::default()
  });

  Guest::push_layer(&started.daemon, &created.id, parts(vec![Ok(b"ab".to_vec()), Ok(b"cd".to_vec())])).await.unwrap();
  let broken = parts(vec![
    Ok(b"xy".to_vec()),
    Err(ControlError::Unavailable("the layer upload broke".into())),
  ]);
  let error = Guest::push_layer(&started.daemon, &created.id, broken).await.unwrap_err();

  // The broken upload leaves the whole one in place, and no partial file.
  assert_eq!(std::fs::read(run.dir.join(UPPER_LAYER)).unwrap(), b"abcd");
  assert!(error.to_string().contains("the layer upload broke"), "{error}");
  let files: Vec<_> = std::fs::read_dir(&run.dir).unwrap().map(|e| e.unwrap().file_name()).collect();
  assert!(!files.iter().any(|f| f.to_string_lossy().ends_with(".partial")), "{files:?}");
}

#[tokio::test]
async fn a_run_whose_upper_layer_is_disposable_refuses_an_upload() {
  let started = testing::daemon().await;
  let created = Runs::create(&started.daemon, new_run(NO_MOUNT, started.dir.path())).await.unwrap();

  let error = Guest::push_layer(&started.daemon, &created.id, parts(vec![Ok(b"ab".to_vec())])).await.unwrap_err();

  assert!(matches!(error, ControlError::Failed(_)), "{error:?}");
  let run = started.daemon.runs.find(&created.id).unwrap();
  assert!(!run.dir.join(UPPER_LAYER).exists());
}
