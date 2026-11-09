use std::sync::Arc;
use std::time::Duration;

use needy_api_server::control::ControlError;
use needy_config::profile::{Executor, OnFull, Runtime};
use needy_core_executor::provider::InstanceSpec;

use super::*;
use crate::testing;

fn spec(name: &str, key: &str, max_procs: Option<u32>, on_full: OnFull) -> InstanceSpec {
  InstanceSpec {
    name: name.into(),
    key: format!("{name}@{key}"),
    session: key.into(),
    config: Executor {
      runtime: Runtime::Native,
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
async fn sessions_with_one_key_share_an_instance_until_the_last_release() {
  let executor = testing::Executor::start().await;
  let pool = Pool::new(executor.native.clone(), None);
  let first = pool.acquire(spec("here", "g", None, OnFull::Wait)).await.unwrap();
  let second = pool.acquire(spec("here", "g", None, OnFull::Wait)).await.unwrap();
  assert!(Arc::ptr_eq(&first, &second));
  assert_eq!(pool.instances().await.len(), 1);

  pool.release(&first).await;
  assert_eq!(pool.instances().await.len(), 1, "one session still holds it");
  pool.release(&second).await;
  assert!(pool.instances().await.is_empty());
}

#[tokio::test]
async fn an_executor_name_with_instances_under_two_keys_is_ambiguous() {
  let executor = testing::Executor::start().await;
  let pool = Pool::new(executor.native.clone(), None);
  let a = pool.acquire(spec("here", "a", None, OnFull::Wait)).await.unwrap();
  let b = pool.acquire(spec("here", "b", None, OnFull::Wait)).await.unwrap();
  let Err(ControlError::Invalid(message)) = pool.named("here").await else {
    panic!("an ambiguous name must be refused");
  };
  assert!(message.contains("here@a, here@b"), "{message}");
  assert!(matches!(pool.named("there").await, Err(ControlError::NotFound(_))));

  pool.release(&a).await;
  assert!(Arc::ptr_eq(&pool.named("here").await.unwrap(), &b));
}

#[tokio::test]
async fn a_full_instance_rejects_with_on_full_reject() {
  let executor = testing::Executor::start().await;
  let pool = Pool::new(executor.native.clone(), None);
  let instance = pool.acquire(spec("here", "s", Some(1), OnFull::Reject)).await.unwrap();
  let held = instance.slot().await.unwrap();
  assert!(held.is_some());
  assert!(matches!(instance.slot().await, Err(ControlError::Exhausted(_))));
  drop(held);
  assert!(instance.slot().await.unwrap().is_some());
}

#[tokio::test]
async fn a_full_instance_makes_a_command_wait_with_on_full_wait() {
  let executor = testing::Executor::start().await;
  let pool = Pool::new(executor.native.clone(), None);
  let instance = pool.acquire(spec("here", "s", Some(1), OnFull::Wait)).await.unwrap();
  let held = instance.slot().await.unwrap();
  let waiting = tokio::spawn({
    let instance = instance.clone();
    async move { instance.slot().await.map(|slot| slot.is_some()) }
  });
  tokio::time::sleep(Duration::from_millis(50)).await;
  assert!(!waiting.is_finished(), "the second command waits for the slot");
  drop(held);
  let taken = tokio::time::timeout(Duration::from_secs(5), waiting).await.unwrap().unwrap();
  assert_eq!(taken, Ok(true));
}

#[tokio::test]
async fn an_instance_without_max_procs_has_no_slots() {
  let executor = testing::Executor::start().await;
  let pool = Pool::new(executor.native.clone(), None);
  let instance = pool.acquire(spec("here", "s", None, OnFull::Reject)).await.unwrap();
  let first = instance.slot().await.unwrap();
  let second = instance.slot().await.unwrap();
  assert!(first.is_none() && second.is_none());
}
