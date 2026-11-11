use super::super::Store;
use super::*;

const MINUTE_MS: i64 = 60_000;
const NOW: i64 = 1_000_000;

async fn store() -> (Store, tempfile::TempDir) {
  let dir = tempfile::tempdir().unwrap();
  let store = Store::open(&dir.path().join("state.db")).await.unwrap();
  (store, dir)
}

/// A client `name` that paired with a fresh code, and its token.
async fn paired(store: &Store, name: &str) -> (Client, Secret) {
  let (_, code) = store.create_pairing_code(name, 10 * MINUTE_MS, NOW).await.unwrap();
  store.pair(code.expose(), NOW).await.unwrap()
}

#[tokio::test]
async fn a_pairing_code_gives_a_client_a_token_that_authenticates_it() {
  let (store, _dir) = store().await;

  let (code, secret) = store.create_pairing_code("laptop", 10 * MINUTE_MS, NOW).await.unwrap();
  let (client, token) = store.pair(secret.expose(), NOW + 1).await.unwrap();

  assert_eq!((code.name.as_str(), code.expires_ms), ("laptop", NOW + 10 * MINUTE_MS));
  assert!(secret.expose().starts_with(&format!("needy-pair.{}.", code.id)));
  assert!(token.expose().starts_with(&format!("needy-client.{}.", client.id)));
  assert!(is_client_token(token.expose()));
  assert_eq!(client.name, "laptop");
  let found = store.authenticate_client(token.expose(), NOW + 2).await.unwrap().unwrap();
  assert_eq!((found.id, found.name), (client.id, "laptop".to_string()));
}

#[tokio::test]
async fn a_pairing_code_is_used_once() {
  let (store, _dir) = store().await;
  let (code, secret) = store.create_pairing_code("laptop", 10 * MINUTE_MS, NOW).await.unwrap();
  store.pair(secret.expose(), NOW).await.unwrap();

  let again = store.pair(secret.expose(), NOW + 1).await.unwrap_err();

  assert!(matches!(again, Error::PairingCodeUsed(ref id) if *id == code.id), "{again}");
}

#[tokio::test]
async fn an_expired_pairing_code_pairs_nobody() {
  let (store, _dir) = store().await;
  let (code, secret) = store.create_pairing_code("laptop", MINUTE_MS, NOW).await.unwrap();

  let late = store.pair(secret.expose(), NOW + MINUTE_MS).await.unwrap_err();

  assert!(matches!(late, Error::PairingCodeExpired(ref id) if *id == code.id), "{late}");
  assert!(store.clients().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_wrong_or_malformed_code_says_only_that_it_is_not_valid() {
  let (store, _dir) = store().await;
  let (code, _) = store.create_pairing_code("laptop", MINUTE_MS, NOW).await.unwrap();

  for guess in [
    format!("needy-pair.{}.00", code.id),
    "needy-pair.nobody.00".into(),
    "garbage".into(),
  ] {
    let refused = store.pair(&guess, NOW).await.unwrap_err();
    assert!(matches!(refused, Error::InvalidPairingCode), "{guess}: {refused}");
  }
}

#[tokio::test]
async fn a_revoked_client_token_authenticates_nobody_and_the_client_stays_listed() {
  let (store, _dir) = store().await;
  let (client, token) = paired(&store, "laptop").await;

  store.revoke_client(&client.id, NOW + 5).await.unwrap();

  assert_eq!(store.authenticate_client(token.expose(), NOW + 6).await.unwrap(), None);
  let listed = store.clients().await.unwrap();
  assert_eq!(listed.len(), 1);
  assert_eq!(listed[0].revoked_ms, Some(NOW + 5));
  assert!(matches!(store.revoke_client("nobody", NOW).await, Err(Error::NoClient(_))));
}

#[tokio::test]
async fn a_wrong_secret_for_a_known_client_authenticates_nobody() {
  let (store, _dir) = store().await;
  let (client, _) = paired(&store, "laptop").await;

  let guess = format!("needy-client.{}.00", client.id);

  assert_eq!(store.authenticate_client(&guess, NOW).await.unwrap(), None);
  assert_eq!(store.authenticate_client("needy-runner.mac.00", NOW).await.unwrap(), None);
}

// A client is identified by its name (owner decision D10, 2026-11-12): the
// name is its principal in the trace (`client:NAME`), the operator gives it
// to the pairing code, and at most one client that is not revoked holds it.
// Pairing again under the name re-pairs that client: the new token replaces
// the old, which is revoked in the same transaction, so a client never holds
// two working tokens.
#[tokio::test]
async fn pairing_again_under_a_clients_name_revokes_its_old_token() {
  let (store, _dir) = store().await;
  let (old, old_token) = paired(&store, "laptop").await;

  let (_, code) = store.create_pairing_code("laptop", MINUTE_MS, NOW + 1).await.unwrap();
  let (new, new_token) = store.pair(code.expose(), NOW + 2).await.unwrap();

  assert_ne!(new.id, old.id);
  assert_eq!(store.authenticate_client(old_token.expose(), NOW + 3).await.unwrap(), None);
  assert_eq!(store.authenticate_client(new_token.expose(), NOW + 3).await.unwrap().map(|c| c.id), Some(new.id.clone()));
  let listed: Vec<(String, Option<i64>)> = store.clients().await.unwrap().into_iter().map(|c| (c.id, c.revoked_ms)).collect();
  assert_eq!(listed, [(old.id, Some(NOW + 2)), (new.id, None)]);
}

// The takeover is explicit (owner decision, 2026-11-12): a code for the name
// of an active client names that client, so the operator is told before
// handing the code out that pairing with it replaces the client.
#[tokio::test]
async fn a_code_for_an_active_clients_name_names_the_client_it_replaces() {
  let (store, _dir) = store().await;
  let (laptop, _) = paired(&store, "laptop").await;
  let (revoked, _) = paired(&store, "old").await;
  store.revoke_client(&revoked.id, NOW).await.unwrap();

  let (replacing, _) = store.create_pairing_code("laptop", MINUTE_MS, NOW + 1).await.unwrap();
  let (fresh, _) = store.create_pairing_code("desk", MINUTE_MS, NOW + 1).await.unwrap();
  let (reused, _) = store.create_pairing_code("old", MINUTE_MS, NOW + 1).await.unwrap();

  assert_eq!(replacing.replaces.map(|c| c.id), Some(laptop.id));
  assert_eq!(fresh.replaces, None);
  assert_eq!(reused.replaces, None, "a revoked client is not replaced");
}

// Without a name a code must never re-pair, and so silently replace, an
// existing client: its generated name is one no client holds, active or
// revoked.
#[tokio::test]
async fn a_code_without_a_name_gets_a_name_no_client_holds() {
  let (store, _dir) = store().await;
  let (laptop, _) = paired(&store, "laptop").await;
  let (gone, _) = paired(&store, "").await;
  store.revoke_client(&gone.id, NOW).await.unwrap();

  let (code, _) = store.create_pairing_code("", MINUTE_MS, NOW + 1).await.unwrap();

  assert!(code.name.starts_with("client-"), "{}", code.name);
  assert_eq!(code.name, format!("client-{}", code.id));
  assert_ne!(code.name, laptop.name);
  assert_ne!(code.name, gone.name);
  assert_eq!(code.replaces, None);
}

#[tokio::test]
async fn a_name_with_an_open_pairing_code_is_refused() {
  let (store, _dir) = store().await;
  store.create_pairing_code("laptop", MINUTE_MS, NOW).await.unwrap();

  let taken = store.create_pairing_code("laptop", MINUTE_MS, NOW).await.unwrap_err();
  store.create_pairing_code("laptop", MINUTE_MS, NOW + MINUTE_MS).await.unwrap();

  assert!(matches!(taken, Error::PairingCodeOpen(ref name) if name == "laptop"), "{taken}");
}

#[tokio::test]
async fn a_derived_name_and_a_given_name_both_pair() {
  let (store, _dir) = store().await;
  let (_, first) = store.create_pairing_code("", MINUTE_MS, NOW).await.unwrap();
  let (_, second) = store.create_pairing_code("ci", MINUTE_MS, NOW).await.unwrap();
  let (one, _) = store.pair(first.expose(), NOW).await.unwrap();
  store.pair(second.expose(), NOW).await.unwrap();

  assert!(one.name.starts_with("client-"), "{}", one.name);
  let names: Vec<String> = store.clients().await.unwrap().into_iter().map(|c| c.name).collect();
  assert_eq!(names.len(), 2);
  assert!(names.contains(&"ci".to_string()), "{names:?}");
}

#[tokio::test]
async fn a_pairing_code_needs_a_positive_ttl_and_a_dns_label_name() {
  let (store, _dir) = store().await;

  assert!(matches!(store.create_pairing_code("laptop", 0, NOW).await, Err(Error::Invalid(_))));
  assert!(matches!(store.create_pairing_code("Neko's Mac", MINUTE_MS, NOW).await, Err(Error::Invalid(_))));
}

// Every request authenticates the client; writing `last_used` on each
// would turn reads into writes, so it is written at most once a minute.
#[tokio::test]
async fn last_used_is_written_at_most_once_a_minute() {
  let (store, _dir) = store().await;
  let (_, token) = paired(&store, "laptop").await;

  store.authenticate_client(token.expose(), NOW + 1_000).await.unwrap();
  store.authenticate_client(token.expose(), NOW + 30_000).await.unwrap();
  let first = store.clients().await.unwrap()[0].last_used_ms;
  store.authenticate_client(token.expose(), NOW + 1_000 + MINUTE_MS).await.unwrap();
  let later = store.clients().await.unwrap()[0].last_used_ms;

  assert_eq!((first, later), (Some(NOW + 1_000), Some(NOW + 1_000 + MINUTE_MS)));
}

#[tokio::test]
async fn only_the_hashes_are_stored() {
  let (store, dir) = store().await;
  let (_, code) = store.create_pairing_code("laptop", MINUTE_MS, NOW).await.unwrap();
  let code_text = code.expose().to_string();
  let (_, token) = store.pair(&code_text, NOW).await.unwrap();
  drop(store);

  let mut bytes = Vec::new();
  for entry in std::fs::read_dir(dir.path()).unwrap() {
    bytes.extend(std::fs::read(entry.unwrap().path()).unwrap());
  }
  let text = String::from_utf8_lossy(&bytes);
  for secret in [code_text.as_str(), token.expose()] {
    let random = secret.rsplit('.').next().unwrap();
    assert!(!text.contains(random), "{secret} is stored in plain text");
  }
}
