use std::sync::Arc;
use std::time::Duration;

use needy_api_server::control::ControlError;
use needy_config::profile::{Executor, ExecutorDriver, OnFull};
use needy_core_executor::driver::Spec;

use super::*;
use crate::testing;

fn spec(name: &str, key: &str, max_procs: Option<u32>, on_full: OnFull) -> Spec {
  Spec {
    name: name.into(),
    key: format!("{name}@{key}"),
    run: key.into(),
    config: Executor {
      driver: ExecutorDriver::Native,
      max_procs,
      on_full,
      ..Executor::default()
    },
    base: std::env::temp_dir(),
    user: Default::default(),
    hostname: None,
  }
}

#[tokio::test]
async fn runs_with_one_key_share_an_instance_until_the_last_release() {
  let executor = testing::Executor::start().await;
  let pool = Pool::new(executor.native.clone(), None, Arc::default());
  let first = pool.acquire(spec("here", "g", None, OnFull::Wait), &Site::ControlHost).await.unwrap();
  let second = pool.acquire(spec("here", "g", None, OnFull::Wait), &Site::ControlHost).await.unwrap();
  assert!(Arc::ptr_eq(&first, &second));
  assert_eq!(pool.instances().await.len(), 1);

  pool.release(&first).await;
  assert_eq!(pool.instances().await.len(), 1, "one run still holds it");
  pool.release(&second).await;
  assert!(pool.instances().await.is_empty());
}

#[tokio::test]
async fn an_executor_name_with_instances_under_two_keys_is_ambiguous() {
  let executor = testing::Executor::start().await;
  let pool = Pool::new(executor.native.clone(), None, Arc::default());
  let a = pool.acquire(spec("here", "a", None, OnFull::Wait), &Site::ControlHost).await.unwrap();
  let b = pool.acquire(spec("here", "b", None, OnFull::Wait), &Site::ControlHost).await.unwrap();
  let Err(ControlError::Invalid(message)) = pool.named("here").await else {
    panic!("an ambiguous name must be refused");
  };
  assert!(message.contains("here@a, here@b"), "{message}");
  assert!(matches!(pool.named("there").await, Err(ControlError::NotFound(_))));

  pool.release(&a).await;
  assert!(Arc::ptr_eq(&pool.named("here").await.unwrap(), &b));
}

/// Exercises the pool's resolved policy through the real process service.
async fn start(instance: &Instance, argv: &[&str]) -> needy_api_client::Result<needy_api_client::exec::ExecStream> {
  let start = needy_api_client::proto::v1::ExecStart {
    argv: argv.iter().map(|s| s.to_string()).collect(),
    detach: true,
    ..Default::default()
  };
  match instance.capacity() {
    Some(capacity) => {
      needy_api_client::exec::ExecStream::start_limited(
        &*instance.handle.executor,
        needy_api_client::proto::v1::LimitedExecStart {
          start: Some(start),
          capacity: Some(capacity),
        },
      )
      .await
    }
    None => needy_api_client::exec::ExecStream::start(&*instance.handle.executor, start).await,
  }
}

#[tokio::test]
async fn a_full_instance_rejects_with_on_full_reject() {
  let executor = testing::Executor::start().await;
  let pool = Pool::new(executor.native.clone(), None, Arc::default());
  let instance = pool.acquire(spec("here", "s", Some(1), OnFull::Reject), &Site::ControlHost).await.unwrap();
  let first = start(&instance, &["sleep", "300"]).await.unwrap();
  let error = start(&instance, &["true"]).await.err().expect("the full instance rejects another command");
  assert_eq!(error.kind(), needy_api_client::Kind::Exhausted);
  first.input().signal("SIGKILL").await.unwrap();
  first.run(None).await.unwrap();
  assert_eq!(start(&instance, &["true"]).await.unwrap().run(None).await.unwrap().exit.code, Some(0));
}

#[tokio::test]
async fn a_full_instance_makes_a_command_wait_with_on_full_wait() {
  let executor = testing::Executor::start().await;
  let pool = Pool::new(executor.native.clone(), None, Arc::default());
  let instance = pool.acquire(spec("here", "s", Some(1), OnFull::Wait), &Site::ControlHost).await.unwrap();
  let first = start(&instance, &["sleep", "300"]).await.unwrap();
  let waiting = tokio::spawn({
    let instance = instance.clone();
    async move { start(&instance, &["true"]).await }
  });
  tokio::time::sleep(Duration::from_millis(50)).await;
  assert!(!waiting.is_finished(), "the second command waits until actual process exit");
  first.input().signal("SIGKILL").await.unwrap();
  first.run(None).await.unwrap();
  let second = tokio::time::timeout(Duration::from_secs(5), waiting).await.unwrap().unwrap().unwrap();
  assert_eq!(second.run(None).await.unwrap().exit.code, Some(0));
}

#[tokio::test]
async fn an_instance_without_max_procs_can_start_two_commands() {
  let executor = testing::Executor::start().await;
  let pool = Pool::new(executor.native.clone(), None, Arc::default());
  let instance = pool.acquire(spec("here", "s", None, OnFull::Reject), &Site::ControlHost).await.unwrap();
  let first = start(&instance, &["sleep", "300"]).await.unwrap();
  let second = start(&instance, &["sleep", "300"]).await.unwrap();
  first.input().signal("SIGKILL").await.unwrap();
  second.input().signal("SIGKILL").await.unwrap();
  assert_eq!(first.run(None).await.unwrap().exit.signal, "SIGKILL");
  assert_eq!(second.run(None).await.unwrap().exit.signal, "SIGKILL");
}

// TODO(test): the regression test for an instance whose creation is slow
// (`Pool::acquire` once held the pool's lock while a provider created an
// instance, so every other key waited) used the `remote` provider and an
// executor socket that came up late. Runners replaced that provider, and the
// providers left create at once (native, a runner's machine) or need Docker;
// the test returns with a provider that can be slowed without Docker.

#[tokio::test]
async fn a_failed_creation_leaves_its_key_free_for_the_next_acquire() {
  let executor = testing::Executor::start().await;
  // No Docker on this pool, so a Docker executor fails to create.
  let pool = Pool::new(executor.native.clone(), None, Arc::default());
  let mut failing = spec("here", "k", None, OnFull::Wait);
  failing.config.driver = ExecutorDriver::LinuxContainer;
  assert!(pool.acquire(failing, &Site::ControlHost).await.is_err());
  assert!(pool.instances().await.is_empty());

  let instance = pool.acquire(spec("here", "k", None, OnFull::Wait), &Site::ControlHost).await.unwrap();
  assert_eq!(instance.handle.driver, "native");
  assert_eq!(pool.instances().await.len(), 1);
}

#[tokio::test]
async fn a_native_executor_on_a_runner_reaches_its_machine_with_its_platforms_sftp_server() {
  let executor = testing::Executor::start().await;
  let pool = Pool::new(executor.native.clone(), None, Arc::default());
  let mut native = spec("mac", "k", None, OnFull::Wait);
  native.config.workspace = Some("/Users/user/ws".into());
  let site = Site::Runner {
    name: "mac".into(),
    os: "macos".into(),
    home: "/Users/user".into(),
  };

  let instance = pool.acquire(native, &site).await.unwrap();

  assert_eq!((instance.handle.driver, instance.handle.id.as_str()), ("runner", "mac"));
  assert_eq!(instance.handle.sftp, ["/usr/libexec/sftp-server"]);
  // A path on the runner's machine, as written.
  assert_eq!(instance.handle.workspace, "/Users/user/ws");
}

// `~` in a native executor's paths on a runner is the home of the user the
// runner runs as, which it reports in its heartbeats; the client and the
// control plane do not know it.
#[tokio::test]
async fn a_native_executor_on_a_runner_expands_tilde_to_the_runners_home() {
  let executor = testing::Executor::start().await;
  let pool = Pool::new(executor.native.clone(), None, Arc::default());
  let mut native = spec("mac", "k", None, OnFull::Wait);
  native.config.workspace = Some("~/project".into());
  native.config.path_map = [("/home/agent/project".to_string(), "~/project".to_string())].into();
  let site = Site::Runner {
    name: "mac".into(),
    os: "macos".into(),
    home: "/Users/user".into(),
  };

  let instance = pool.acquire(native, &site).await.unwrap();

  assert_eq!(instance.handle.workspace, "/Users/user/project");
  assert_eq!(instance.path_map["/home/agent/project"], "/Users/user/project");
  assert_eq!(instance.expand_home("~/project/a.txt"), "/Users/user/project/a.txt");
}

#[tokio::test]
async fn a_linux_container_executor_is_never_created_on_a_runner_site() {
  let executor = testing::Executor::start().await;
  let pool = Pool::new(executor.native.clone(), None, Arc::default());
  let mut docker = spec("build", "k", None, OnFull::Wait);
  docker.config.driver = ExecutorDriver::LinuxContainer;
  let site = Site::Runner {
    name: "mac".into(),
    os: "macos".into(),
    home: "/Users/user".into(),
  };

  let error = pool.acquire(docker, &site).await.err().unwrap().to_string();

  assert_eq!(error, "runner mac cannot create linux-container executors yet");
}
