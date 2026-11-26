use std::path::{Path, PathBuf};

use needy_api_client::Client;
use needy_api_client::proto::v1::{
  ClientAccess, CreatePairingCodeRequest, GetCallerRequest, ListClientsRequest, ListJoinTokensRequest, ListRunsRequest, PairRequest,
  RevokeClientRequest,
};
use tonic::Code;

use crate::store;
use crate::testing::{self, Served};

const CONTROL_TOKEN: &str = "control-secret";
const ADMIN_TOKEN: &str = "needy-client.static-controller.0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const RUNS_TOKEN: &str = "needy-client.static-ci.fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";
const ROTATED_TOKEN: &str = "needy-client.static-controller.aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/// A daemon whose clients file starts as `text`, and the file's path.
async fn served(text: &str) -> (Served, PathBuf) {
  let dir = tempfile::tempdir().unwrap();
  let file = dir.path().join("clients.yaml");
  std::fs::write(&file, text).unwrap();
  let started = testing::daemon_in(dir, |config| config.clients_file = Some(file.clone())).await;
  (testing::serve(started, Some(CONTROL_TOKEN)).await, file)
}

/// Replaces the clients file the way a mounted Secret changes: a new inode.
fn replace(file: &Path, text: &str) {
  let tmp = file.with_extension("tmp");
  std::fs::write(&tmp, text).unwrap();
  std::fs::rename(&tmp, file).unwrap();
}

fn with(served: &Served, token: &str) -> Client {
  served.client.clone().with_token(token).unwrap()
}

#[tokio::test]
async fn a_static_admin_token_authenticates_as_an_admin_client() {
  let text = format!("clients:\n  - name: controller\n    access: admin\n    token: {ADMIN_TOKEN}\n");
  let (served, _file) = served(&text).await;
  let admin = with(&served, ADMIN_TOKEN);

  let caller = admin.clients().get_caller(GetCallerRequest {}).await.unwrap().into_inner();
  admin.runners().list_join_tokens(ListJoinTokensRequest {}).await.unwrap();
  let listed = admin.clients().list_clients(ListClientsRequest {}).await.unwrap().into_inner().clients;

  assert_eq!((caller.client_id.as_str(), caller.name.as_str()), ("static-controller", "controller"));
  assert_eq!(caller.access, ClientAccess::Admin as i32);
  assert_eq!(listed.len(), 1);
  assert_eq!((listed[0].id.as_str(), listed[0].name.as_str()), ("static-controller", "controller"));
  assert_eq!(listed[0].access, ClientAccess::Admin as i32);
  assert!(listed[0].last_used_at_ms > 0, "the calls used the token");
}

#[tokio::test]
async fn a_static_runs_client_cannot_call_admin_methods() {
  let hash = store::hex(&store::hash_secret(RUNS_TOKEN));
  let text = format!("clients:\n  - name: ci\n    token_hash: sha256:{hash}\n");
  let (served, _file) = served(&text).await;
  let ci = with(&served, RUNS_TOKEN);

  ci.runs().list_runs(ListRunsRequest {}).await.unwrap();
  let caller = ci.clients().get_caller(GetCallerRequest {}).await.unwrap().into_inner();
  let denied = ci.clients().list_clients(ListClientsRequest {}).await.unwrap_err();

  assert_eq!(caller.access, ClientAccess::Runs as i32);
  assert_eq!(denied.code(), Code::PermissionDenied);
}

#[tokio::test]
async fn a_wrong_token_for_a_static_client_is_refused() {
  let text = format!("clients:\n  - name: controller\n    access: admin\n    token: {ADMIN_TOKEN}\n");
  let (served, _file) = served(&text).await;

  let refused = with(&served, ROTATED_TOKEN).runs().list_runs(ListRunsRequest {}).await.unwrap_err();

  assert_eq!(refused.code(), Code::Unauthenticated);
}

#[tokio::test]
async fn unknown_keys_in_the_clients_file_load_no_clients() {
  for text in [
    format!("clients:\n  - name: controller\n    access: admin\n    token: {ADMIN_TOKEN}\nrunners: []\n"),
    format!("clients:\n  - name: controller\n    access: admin\n    secret: x\n    token: {ADMIN_TOKEN}\n"),
    format!("clients:\n  - name: controller\n    access: root\n    token: {ADMIN_TOKEN}\n"),
  ] {
    let (served, _file) = served(&text).await;

    let refused = with(&served, ADMIN_TOKEN).runs().list_runs(ListRunsRequest {}).await.unwrap_err();

    assert_eq!(refused.code(), Code::Unauthenticated, "{text}");
  }
}

#[tokio::test]
async fn a_token_of_another_static_client_name_is_rejected_when_loading() {
  let text = format!("clients:\n  - name: ci\n    token: {ADMIN_TOKEN}\n");
  let (served, _file) = served(&text).await;

  let refused = with(&served, ADMIN_TOKEN).runs().list_runs(ListRunsRequest {}).await.unwrap_err();

  assert_eq!(refused.code(), Code::Unauthenticated);
}

#[tokio::test]
async fn reloading_the_clients_file_adds_removes_and_rotates_access() {
  let text = format!("clients:\n  - name: controller\n    access: admin\n    token: {ADMIN_TOKEN}\n");
  let (served, file) = served(&text).await;
  with(&served, ADMIN_TOKEN).runs().list_runs(ListRunsRequest {}).await.unwrap();

  replace(&file, &format!("clients:\n  - name: ci\n    token: {RUNS_TOKEN}\n"));
  with(&served, RUNS_TOKEN).runs().list_runs(ListRunsRequest {}).await.unwrap();
  let removed = with(&served, ADMIN_TOKEN).runs().list_runs(ListRunsRequest {}).await.unwrap_err();

  replace(&file, &format!("clients:\n  - name: controller\n    access: admin\n    token: {ROTATED_TOKEN}\n"));
  with(&served, ROTATED_TOKEN).runs().list_runs(ListRunsRequest {}).await.unwrap();
  let rotated_away = with(&served, ADMIN_TOKEN).runs().list_runs(ListRunsRequest {}).await.unwrap_err();
  let dropped = with(&served, RUNS_TOKEN).runs().list_runs(ListRunsRequest {}).await.unwrap_err();

  std::fs::remove_file(&file).unwrap();
  let absent = with(&served, ROTATED_TOKEN).runs().list_runs(ListRunsRequest {}).await.unwrap_err();

  assert_eq!(removed.code(), Code::Unauthenticated);
  assert_eq!(rotated_away.code(), Code::Unauthenticated);
  assert_eq!(dropped.code(), Code::Unauthenticated);
  assert_eq!(absent.code(), Code::Unauthenticated);
}

#[tokio::test]
async fn a_static_client_cannot_be_revoked_through_the_api() {
  let text = format!("clients:\n  - name: controller\n    access: admin\n    token: {ADMIN_TOKEN}\n");
  let (served, _file) = served(&text).await;
  let operator = with(&served, CONTROL_TOKEN);

  let refused = operator
    .clients()
    .revoke_client(RevokeClientRequest {
      id: "static-controller".into(),
    })
    .await
    .unwrap_err();
  with(&served, ADMIN_TOKEN).runs().list_runs(ListRunsRequest {}).await.unwrap();

  assert_eq!(refused.code(), Code::FailedPrecondition);
  assert!(refused.message().contains("clients file"), "{}", refused.message());
}

// The clients file is the authority for a name: a pairing code is not
// created for a static name, and a paired client whose name the file takes
// later is refused while the file holds it, so `client:NAME` in the trace
// names one principal.
#[tokio::test]
async fn a_static_client_name_is_not_shared_with_a_paired_client() {
  let (served, file) = served("clients: []\n").await;
  let operator = with(&served, CONTROL_TOKEN);
  let request = |name: &str| CreatePairingCodeRequest {
    ttl_ms: 60_000,
    name: name.into(),
    access: ClientAccess::Runs as i32,
  };
  let code = operator.clients().create_pairing_code(request("ci")).await.unwrap().into_inner().code;
  let paired = served.client.clients().pair(PairRequest { code }).await.unwrap().into_inner().token;
  with(&served, &paired).runs().list_runs(ListRunsRequest {}).await.unwrap();

  replace(&file, &format!("clients:\n  - name: ci\n    token: {RUNS_TOKEN}\n"));
  let shadowed = with(&served, &paired).runs().list_runs(ListRunsRequest {}).await.unwrap_err();
  let refused_code = operator.clients().create_pairing_code(request("ci")).await.unwrap_err();
  let caller = with(&served, RUNS_TOKEN).clients().get_caller(GetCallerRequest {}).await.unwrap().into_inner();

  replace(&file, "clients: []\n");
  with(&served, &paired).runs().list_runs(ListRunsRequest {}).await.unwrap();

  assert_eq!(shadowed.code(), Code::Unauthenticated);
  assert_eq!(refused_code.code(), Code::FailedPrecondition);
  assert_eq!(caller.client_id, "static-ci");
}

// Unprefixed tokens: plain random secrets an orchestrator generates without
// needy's token format, found by their hash.
const PLAIN_ADMIN: &str = "plain-admin-token-0123456789abcdefghijklmnopqrstuvwxyz";
const PLAIN_RUNS: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const PLAIN_OTHER: &str = "plain-other-token-0123456789abcdefghijklmnopqrstuvwxyz";

#[tokio::test]
async fn an_unprefixed_static_token_authenticates_with_its_access() {
  let hash = store::hex(&store::hash_secret(PLAIN_RUNS));
  let text = format!(
    "clients:\n  - name: controller\n    access: admin\n    token: {PLAIN_ADMIN}\n  - name: batch\n    token_hash: {hash}\n  - name: ci\n    token: {RUNS_TOKEN}\n"
  );
  let (served, _file) = served(&text).await;
  let admin = with(&served, PLAIN_ADMIN);
  let batch = with(&served, PLAIN_RUNS);

  let admin_caller = admin.clients().get_caller(GetCallerRequest {}).await.unwrap().into_inner();
  let listed = admin.clients().list_clients(ListClientsRequest {}).await.unwrap().into_inner().clients;
  let batch_caller = batch.clients().get_caller(GetCallerRequest {}).await.unwrap().into_inner();
  batch.runs().list_runs(ListRunsRequest {}).await.unwrap();
  let denied = batch.clients().list_clients(ListClientsRequest {}).await.unwrap_err();
  let legacy = with(&served, RUNS_TOKEN).clients().get_caller(GetCallerRequest {}).await.unwrap().into_inner();
  let wrong = with(&served, PLAIN_OTHER).runs().list_runs(ListRunsRequest {}).await.unwrap_err();

  assert_eq!((admin_caller.client_id.as_str(), admin_caller.name.as_str()), ("static-controller", "controller"));
  assert_eq!(admin_caller.access, ClientAccess::Admin as i32);
  let controller = listed.iter().find(|client| client.id == "static-controller").unwrap();
  assert!(controller.last_used_at_ms > 0, "the calls used the unprefixed token");
  assert_eq!((batch_caller.client_id.as_str(), batch_caller.name.as_str()), ("static-batch", "batch"));
  assert_eq!(batch_caller.access, ClientAccess::Runs as i32);
  assert_eq!(denied.code(), Code::PermissionDenied);
  assert_eq!(legacy.client_id, "static-ci", "a prefixed token still names its client");
  assert_eq!(wrong.code(), Code::Unauthenticated);
}

#[tokio::test]
async fn a_clients_file_with_a_duplicate_or_weak_token_loads_no_clients() {
  let hash = store::hex(&store::hash_secret(PLAIN_ADMIN));
  let runner_credential = "needy-runner.pod.0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
  for text in [
    format!("clients:\n  - name: a\n    token: {PLAIN_ADMIN}\n  - name: b\n    token: {PLAIN_ADMIN}\n"),
    format!("clients:\n  - name: a\n    token: {PLAIN_ADMIN}\n  - name: b\n    token_hash: {hash}\n"),
    format!("clients:\n  - name: a\n    token: {PLAIN_ADMIN}\n  - name: b\n    token: {}\n", &PLAIN_OTHER[..42]),
    format!("clients:\n  - name: a\n    token: {PLAIN_ADMIN}\n  - name: b\n    token: {}\n", &PLAIN_RUNS[..63]),
    format!("clients:\n  - name: a\n    token: {PLAIN_ADMIN}\n  - name: b\n    token: {runner_credential}\n"),
  ] {
    let (served, _file) = served(&text).await;

    let refused = with(&served, PLAIN_ADMIN).runs().list_runs(ListRunsRequest {}).await.unwrap_err();

    assert_eq!(refused.code(), Code::Unauthenticated, "{text}");
  }
}

#[tokio::test]
async fn reloading_the_clients_file_adds_and_removes_unprefixed_tokens() {
  let (served, file) = served(&format!("clients:\n  - name: controller\n    token: {PLAIN_ADMIN}\n")).await;
  with(&served, PLAIN_ADMIN).runs().list_runs(ListRunsRequest {}).await.unwrap();

  replace(&file, &format!("clients:\n  - name: ci\n    token: {PLAIN_RUNS}\n"));
  let added = with(&served, PLAIN_RUNS).clients().get_caller(GetCallerRequest {}).await.unwrap().into_inner();
  let removed = with(&served, PLAIN_ADMIN).runs().list_runs(ListRunsRequest {}).await.unwrap_err();

  replace(&file, &format!("clients:\n  - name: ci\n    token: {PLAIN_OTHER}\n"));
  with(&served, PLAIN_OTHER).runs().list_runs(ListRunsRequest {}).await.unwrap();
  let rotated_away = with(&served, PLAIN_RUNS).runs().list_runs(ListRunsRequest {}).await.unwrap_err();

  assert_eq!(added.client_id, "static-ci");
  assert_eq!(removed.code(), Code::Unauthenticated);
  assert_eq!(rotated_away.code(), Code::Unauthenticated);
}
