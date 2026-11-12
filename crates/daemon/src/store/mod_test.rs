use std::collections::{BTreeMap, BTreeSet};

use needy_config::profile::{ActorDriver, ExecutorDriver};
use needy_core::types::{Capabilities, RunnerRole};

use super::*;

const HOUR_MS: i64 = 3_600_000;
const NOW: i64 = 1_000_000;

async fn store() -> (Store, tempfile::TempDir) {
  let dir = tempfile::tempdir().unwrap();
  let store = Store::open(&dir.path().join("state.db")).await.unwrap();
  (store, dir)
}

fn roles(roles: &[RunnerRole]) -> BTreeSet<RunnerRole> {
  roles.iter().copied().collect()
}

fn native() -> Capabilities {
  Capabilities {
    executor: [ExecutorDriver::Native].into(),
    ..Default::default()
  }
}

fn firecracker() -> Capabilities {
  Capabilities {
    actor: [ActorDriver::LinuxFirecracker].into(),
    ..Default::default()
  }
}

fn kvm() -> Capabilities {
  Capabilities {
    actor: [ActorDriver::LinuxFirecracker].into(),
    executor: [ExecutorDriver::Native, ExecutorDriver::LinuxContainer].into(),
  }
}

fn labels(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
  pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn any_scope(uses: u32) -> NewJoinToken {
  NewJoinToken {
    scope: Scope::default(),
    ttl_ms: HOUR_MS,
    uses,
  }
}

fn executor() -> Report {
  Report {
    capabilities: native(),
    labels: BTreeMap::new(),
  }
}

/// A runner `name` that joined with a fresh single-use token, and its
/// credential.
async fn joined(store: &Store, name: &str) -> Secret {
  let (_, token) = store.create_join_token(any_scope(1), NOW).await.unwrap();
  store.join(token.expose(), name, executor(), NOW).await.unwrap().1
}

#[tokio::test]
async fn a_join_token_is_listed_without_its_secret_and_stored_hashed() {
  let (store, dir) = store().await;

  let (created, token) = store.create_join_token(any_scope(2), NOW).await.unwrap();

  assert!(token.expose().starts_with(&format!("needy-join.{}.", created.id)), "{}", token.expose());
  assert_eq!(created.expires_ms, NOW + HOUR_MS);
  assert_eq!(created.uses_left, 2);
  assert_eq!(store.join_tokens().await.unwrap(), [created]);
  let secret = token.expose().rsplit('.').next().unwrap();
  let database = std::fs::read(dir.path().join("state.db")).unwrap();
  assert!(!database.windows(secret.len()).any(|w| w == secret.as_bytes()), "the secret is not stored in plain text");
}

#[tokio::test]
async fn joining_returns_a_credential_that_authenticates_the_runner() {
  let (store, _dir) = store().await;
  let (_, token) = store.create_join_token(any_scope(1), NOW).await.unwrap();

  let (runner, credential) = store.join(token.expose(), "mac", executor(), NOW + 5).await.unwrap();

  assert_eq!(runner.name, "mac");
  assert_eq!(runner.capabilities, native());
  assert_eq!(runner.last_seen_ms, NOW + 5);
  assert_eq!(store.authenticate(credential.expose()).await.unwrap().as_deref(), Some("mac"));
  assert_eq!(store.authenticate("needy-runner.mac.0000").await.unwrap(), None);
  assert_eq!(store.authenticate(token.expose()).await.unwrap(), None, "a join token is no credential");
}

#[tokio::test]
async fn an_expired_join_token_is_refused() {
  let (store, _dir) = store().await;
  let (created, token) = store.create_join_token(any_scope(1), NOW).await.unwrap();

  let error = store.join(token.expose(), "mac", executor(), NOW + HOUR_MS).await.unwrap_err();

  assert_eq!(error.to_string(), format!("join token {} expired", created.id));
  assert!(store.runners().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_join_token_is_refused_once_its_uses_are_spent() {
  let (store, _dir) = store().await;
  let (created, token) = store.create_join_token(any_scope(2), NOW).await.unwrap();
  store.join(token.expose(), "one", executor(), NOW).await.unwrap();
  store.join(token.expose(), "two", executor(), NOW).await.unwrap();

  let error = store.join(token.expose(), "three", executor(), NOW).await.unwrap_err();

  assert_eq!(error.to_string(), format!("join token {} has no uses left", created.id));
  assert_eq!(store.join_tokens().await.unwrap()[0].uses_left, 0);
}

#[tokio::test]
async fn a_revoked_join_token_is_refused() {
  let (store, _dir) = store().await;
  let (created, token) = store.create_join_token(any_scope(1), NOW).await.unwrap();

  store.revoke_join_token(&created.id, NOW + 1).await.unwrap();
  let error = store.join(token.expose(), "mac", executor(), NOW + 2).await.unwrap_err();

  assert_eq!(error.to_string(), format!("join token {} was revoked", created.id));
  assert_eq!(store.join_tokens().await.unwrap()[0].revoked_ms, Some(NOW + 1));
}

#[tokio::test]
async fn a_join_token_with_a_wrong_secret_says_nothing_about_the_token() {
  let (store, _dir) = store().await;
  let (created, _) = store.create_join_token(any_scope(1), NOW).await.unwrap();
  store.revoke_join_token(&created.id, NOW).await.unwrap();
  let guess = format!("needy-join.{}.{}", created.id, "0".repeat(64));

  let error = store.join(&guess, "mac", executor(), NOW).await.unwrap_err();

  assert_eq!(error.to_string(), "the join token is not valid");
  assert_eq!(store.join("garbage", "mac", executor(), NOW).await.unwrap_err().to_string(), "the join token is not valid");
}

#[tokio::test]
async fn a_failed_join_does_not_spend_a_use() {
  let (store, _dir) = store().await;
  joined(&store, "mac").await;
  let (_, token) = store.create_join_token(any_scope(1), NOW).await.unwrap();

  let error = store.join(token.expose(), "mac", executor(), NOW).await.unwrap_err();

  assert_eq!(error.to_string(), "runner mac exists; remove it first or join with another name");
  store.join(token.expose(), "other", executor(), NOW).await.unwrap();
}

#[tokio::test]
async fn a_join_token_limits_the_roles_and_assigns_its_labels() {
  let (store, _dir) = store().await;
  let scoped = NewJoinToken {
    scope: Scope {
      roles: roles(&[RunnerRole::Executor]),
      labels: labels(&[("os", "darwin")]),
    },
    ttl_ms: HOUR_MS,
    uses: 3,
  };
  let (_, token) = store.create_join_token(scoped, NOW).await.unwrap();
  let actor = Report {
    capabilities: kvm(),
    labels: BTreeMap::new(),
  };
  let conflicting = Report {
    capabilities: native(),
    labels: labels(&[("os", "linux")]),
  };
  let fitting = Report {
    capabilities: native(),
    labels: labels(&[("arch", "arm64")]),
  };

  let too_many_roles = store.join(token.expose(), "a", actor, NOW).await.unwrap_err();
  let other_label = store.join(token.expose(), "b", conflicting, NOW).await.unwrap_err();
  let (runner, _) = store.join(token.expose(), "c", fitting, NOW).await.unwrap();

  assert_eq!(too_many_roles.to_string(), "the join token does not allow the role actor; start the runner with --roles executor");
  assert_eq!(other_label.to_string(), "the join token sets the label os=darwin, not os=linux");
  assert_eq!(runner.labels, labels(&[("arch", "arm64"), ("os", "darwin")]));
  assert_eq!(store.join_tokens().await.unwrap()[0].uses_left, 2, "refused joins spend no use");
}

#[tokio::test]
async fn a_heartbeat_updates_last_seen_capabilities_and_labels_within_the_scope() {
  let (store, _dir) = store().await;
  let scoped = NewJoinToken {
    scope: Scope {
      roles: roles(&[RunnerRole::Executor]),
      labels: labels(&[("os", "darwin")]),
    },
    ttl_ms: HOUR_MS,
    uses: 1,
  };
  let (_, token) = store.create_join_token(scoped, NOW).await.unwrap();
  store.join(token.expose(), "mac", executor(), NOW).await.unwrap();
  let report = Report {
    capabilities: kvm().within(&roles(&[RunnerRole::Executor])),
    labels: labels(&[("gpu", "none")]),
  };

  let rotated = store.heartbeat("mac", report, NOW + 15_000).await.unwrap();
  let outside = Report {
    capabilities: firecracker(),
    labels: BTreeMap::new(),
  };
  let refused = store.heartbeat("mac", outside, NOW + 30_000).await.unwrap_err();

  assert!(rotated.is_none());
  assert_eq!(refused.to_string(), "the join token does not allow the role actor");
  let runner = &store.runners().await.unwrap()[0];
  assert_eq!(runner.last_seen_ms, NOW + 15_000);
  assert_eq!(runner.labels, labels(&[("gpu", "none"), ("os", "darwin")]));
  assert_eq!(runner.capabilities.to_string(), "executor(linux-container,native)");
}

#[tokio::test]
async fn rotation_hands_out_a_credential_at_the_next_heartbeat_and_drops_the_old_one_once_used() {
  let (store, _dir) = store().await;
  let old = joined(&store, "mac").await;

  store.rotate("mac").await.unwrap();
  assert!(store.runners().await.unwrap()[0].rotating);
  let new = store.heartbeat("mac", executor(), NOW + 1).await.unwrap().expect("a new credential");

  assert_ne!(new.expose(), old.expose());
  assert_eq!(store.authenticate(old.expose()).await.unwrap().as_deref(), Some("mac"), "the old credential works until the new one is used");
  assert_eq!(store.authenticate(new.expose()).await.unwrap().as_deref(), Some("mac"));
  assert_eq!(store.authenticate(old.expose()).await.unwrap(), None, "the old credential is refused once the new one was used");
  assert!(!store.runners().await.unwrap()[0].rotating);
  assert!(store.heartbeat("mac", executor(), NOW + 2).await.unwrap().is_none());
}

#[tokio::test]
async fn a_rotated_credential_that_was_lost_is_replaced_at_the_next_heartbeat() {
  let (store, _dir) = store().await;
  let old = joined(&store, "mac").await;
  store.rotate("mac").await.unwrap();
  let lost = store.heartbeat("mac", executor(), NOW + 1).await.unwrap().unwrap();

  let again = store.heartbeat("mac", executor(), NOW + 2).await.unwrap().expect("another credential");

  assert_eq!(store.authenticate(lost.expose()).await.unwrap(), None);
  assert_eq!(store.authenticate(again.expose()).await.unwrap().as_deref(), Some("mac"));
  assert_eq!(store.authenticate(old.expose()).await.unwrap(), None);
}

#[tokio::test]
async fn a_removed_runner_is_refused_listed_as_removed_and_may_join_again() {
  let (store, _dir) = store().await;
  let credential = joined(&store, "mac").await;

  store.remove("mac", NOW + 7).await.unwrap();

  assert_eq!(store.authenticate(credential.expose()).await.unwrap(), None);
  assert_eq!(store.runners().await.unwrap()[0].removed_ms, Some(NOW + 7));
  assert_eq!(store.rotate("mac").await.unwrap_err().to_string(), "no runner mac");
  assert_eq!(store.remove("mac", NOW + 8).await.unwrap_err().to_string(), "runner mac is removed; it cannot become removed");
  assert_eq!(store.runners().await.unwrap()[0].removed_ms, Some(NOW + 7), "the first removal stays");
  let again = joined(&store, "mac").await;
  assert_eq!(store.authenticate(again.expose()).await.unwrap().as_deref(), Some("mac"));
  assert_eq!(store.runners().await.unwrap()[0].removed_ms, None);
}

#[tokio::test]
async fn names_labels_and_token_limits_are_checked() {
  let (store, _dir) = store().await;
  let (_, token) = store.create_join_token(any_scope(5), NOW).await.unwrap();
  let bad_label = Report {
    capabilities: native(),
    labels: labels(&[("a b", "c")]),
  };
  let no_roles = Report {
    capabilities: Capabilities::default(),
    labels: BTreeMap::new(),
  };

  let name = store.join(token.expose(), "Mac.local", executor(), NOW).await.unwrap_err();
  let label = store.join(token.expose(), "mac", bad_label, NOW).await.unwrap_err();
  let roleless = store.join(token.expose(), "mac", no_roles, NOW).await.unwrap_err();
  let ttl = store
    .create_join_token(
      NewJoinToken {
        ttl_ms: 0,
        ..any_scope(1)
      },
      NOW,
    )
    .await
    .unwrap_err();
  let uses = store.create_join_token(any_scope(0), NOW).await.unwrap_err();

  assert_eq!(
    name.to_string(),
    "invalid runner name \"Mac.local\": use 1 to 63 lowercase letters, digits and '-', starting and ending with a letter or digit"
  );
  assert_eq!(label.to_string(), "invalid label key \"a b\": use 1 to 63 letters, digits, '.', '_', '-' and '/'");
  assert_eq!(roleless.to_string(), "a runner needs at least one role");
  assert_eq!(ttl.to_string(), "a join token needs a positive time to live");
  assert_eq!(uses.to_string(), "a join token needs at least one use");
}

#[tokio::test]
async fn a_database_another_schema_wrote_is_refused() {
  let dir = tempfile::tempdir().unwrap();
  let path = dir.path().join("state.db");
  // The trace database has migrations this store does not know.
  needy_trace::sink::sqlite::Store::open(&path).await.unwrap();

  let error = Store::open(&path).await.err().unwrap();

  assert!(matches!(error, Error::Schema(_)), "{error}");
}

// A state.db from before capabilities has a `roles` column; the migration
// replaces it, and the runner reports its capabilities at its next
// heartbeat.
#[tokio::test]
async fn a_runner_registry_from_before_capabilities_migrates_and_takes_them_at_the_next_heartbeat() {
  let (store, dir) = store().await;
  joined(&store, "mac").await;
  migration::Migrator::down(&store.db, Some(1)).await.unwrap();
  drop(store);

  let store = Store::open(&dir.path().join("state.db")).await.unwrap();
  let migrated = store.runners().await.unwrap()[0].capabilities.clone();
  store.heartbeat("mac", executor(), NOW + 1).await.unwrap();

  assert_eq!(migrated, Capabilities::default());
  assert_eq!(store.runners().await.unwrap()[0].capabilities, native());
}
