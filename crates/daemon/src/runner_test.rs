use std::collections::BTreeMap;
use std::time::Duration;

use needy_api_client::Client;
use needy_api_client::proto::v1::{
  CreateJoinTokenRequest, ListRunnersRequest, RemoveRunnerRequest, RotateRunnerRequest, Runner as Listed, RunnerStatus,
};
use tokio_util::sync::CancellationToken;

use super::*;
use crate::testing::{self, Served};

const CONTROL_TOKEN: &str = "control-secret";

async fn control(heartbeat: Duration) -> Served {
  testing::serve(testing::daemon_with_heartbeat(heartbeat).await, Some(CONTROL_TOKEN)).await
}

fn operator(served: &Served) -> Client {
  served.client.clone().with_token(CONTROL_TOKEN).unwrap()
}

async fn join_token(served: &Served) -> String {
  let request = CreateJoinTokenRequest {
    ttl_ms: 60_000,
    uses: 1,
    ..Default::default()
  };
  operator(served).runners().create_join_token(request).await.unwrap().into_inner().token
}

async fn listed(served: &Served) -> Vec<Listed> {
  operator(served).runners().list_runners(ListRunnersRequest {}).await.unwrap().into_inner().runners
}

fn config(served: &Served, state_dir: &std::path::Path, join_token: Option<String>) -> Config {
  Config {
    state_dir: state_dir.to_path_buf(),
    control: Some(served.endpoint.clone()),
    join_token,
    credential: None,
    name: "mac".into(),
    capabilities: crate::testing::capabilities("executor(native)"),
    labels: BTreeMap::from([("os".to_string(), "darwin".to_string())]),
    grace: crate::runner::DISCONNECT_GRACE,
    activity_hook: None,
  }
}

/// Waits until `check` holds, for at most 10 s.
async fn eventually(what: &str, mut check: impl AsyncFnMut() -> bool) {
  let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
  while !check().await {
    assert!(tokio::time::Instant::now() < deadline, "{what}");
    tokio::time::sleep(Duration::from_millis(20)).await;
  }
}

#[tokio::test]
async fn a_runner_joins_once_keeps_its_credential_private_and_reuses_it_after_a_restart() {
  let served = control(Duration::from_secs(15)).await;
  let state = tempfile::tempdir().unwrap();
  let token = join_token(&served).await;

  let mut first = Runner::start(config(&served, state.path(), Some(token))).await.unwrap();
  first.beat().await.unwrap();
  let stored = std::fs::read_to_string(state.path().join("runner.json")).unwrap();
  let before = listed(&served).await[0].last_seen_at_ms;
  tokio::time::sleep(Duration::from_millis(5)).await;
  // A restart has no join token; the one it joined with is spent anyway.
  let mut second = Runner::start(config(&served, state.path(), None)).await.unwrap();
  second.beat().await.unwrap();

  assert!(stored.contains("needy-runner.mac."), "the credential is stored");
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(state.path().join("runner.json")).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
  }
  let runners = listed(&served).await;
  assert_eq!(runners.len(), 1);
  assert_eq!((runners[0].name.as_str(), runners[0].status()), ("mac", RunnerStatus::Online));
  assert!(runners[0].last_seen_at_ms > before, "the restarted runner's heartbeat arrived");
}

#[tokio::test]
async fn a_first_start_without_a_join_token_says_what_it_needs() {
  let served = control(Duration::from_secs(15)).await;
  let state = tempfile::tempdir().unwrap();

  let error = Runner::start(config(&served, state.path(), None)).await.err().unwrap();

  assert_eq!(error.to_string(), "this runner has not joined yet: give it a join token with --join-token-env or --join-token-file");
}

#[tokio::test]
async fn a_restart_under_another_name_is_refused() {
  let served = control(Duration::from_secs(15)).await;
  let state = tempfile::tempdir().unwrap();
  Runner::start(config(&served, state.path(), Some(join_token(&served).await))).await.unwrap();
  let renamed = Config {
    name: "other".into(),
    ..config(&served, state.path(), None)
  };

  let error = Runner::start(renamed).await.err().unwrap();

  assert!(error.to_string().starts_with("this runner joined as mac, not other"), "{error}");
}

#[tokio::test]
async fn heartbeats_pick_up_a_rotated_credential_and_end_once_the_runner_is_removed() {
  let served = control(Duration::from_millis(100)).await;
  let state = tempfile::tempdir().unwrap();
  let runner = Runner::start(config(&served, state.path(), Some(join_token(&served).await))).await.unwrap();
  let path = state.path().join("runner.json");
  let old = std::fs::read_to_string(&path).unwrap();
  let stop = CancellationToken::new();
  let beating = tokio::spawn(runner.run(stop.clone()));

  operator(&served).runners().rotate_runner(RotateRunnerRequest { name: "mac".into() }).await.unwrap();
  eventually("the rotated credential is stored and used", async || {
    std::fs::read_to_string(&path).unwrap() != old && !listed(&served).await[0].rotating
  })
  .await;
  operator(&served).runners().remove_runner(RemoveRunnerRequest { name: "mac".into() }).await.unwrap();
  let ended = tokio::time::timeout(Duration::from_secs(10), beating).await.expect("heartbeats end").unwrap();

  let error = ended.unwrap_err().to_string();
  assert!(error.starts_with("the control plane refused this runner's credential"), "{error}");
  assert_eq!(listed(&served).await[0].status(), RunnerStatus::Removed);
}

#[tokio::test]
async fn enrollment_saves_role_bounds_and_settings_without_starting_the_runner() {
  let served = control(Duration::from_secs(15)).await;
  let state = tempfile::tempdir().unwrap();
  let settings = Settings {
    roles: None,
    labels: BTreeMap::from([("os".into(), "darwin".into())]),
    actor_image: Some(std::path::PathBuf::from("/home/user/actor-image")),
  };
  enroll(config(&served, state.path(), Some(join_token(&served).await)), settings).await.unwrap();
  let (name, saved) = enrollment(state.path()).unwrap();
  assert_eq!(name, "mac");
  assert_eq!(saved.roles, Some(BTreeSet::from([needy_core::types::RunnerRole::Executor])));
  assert_eq!(saved.labels.get("os").map(String::as_str), Some("darwin"));
  assert_eq!(saved.actor_image.as_deref(), Some(std::path::Path::new("/home/user/actor-image")));
  let error = enroll(config(&served, state.path(), None), Settings::default()).await.unwrap_err();
  assert!(error.to_string().contains("already joined"));
  let mut runner = Runner::start(config(&served, state.path(), None)).await.unwrap();
  runner.beat().await.unwrap();
  assert_eq!(listed(&served).await.len(), 1);
}

#[tokio::test]
async fn rejected_label_overrides_do_not_replace_saved_runner_settings() {
  let served = control(Duration::from_secs(15)).await;
  let state = tempfile::tempdir().unwrap();
  let settings = Settings {
    labels: BTreeMap::from([("os".into(), "darwin".into())]),
    ..Settings::default()
  };
  enroll(config(&served, state.path(), Some(join_token(&served).await)), settings).await.unwrap();
  let mut overridden = config(&served, state.path(), None);
  overridden.labels = BTreeMap::from([("invalid key".into(), "value".into())]);
  let error = serve_with_labels(overridden, true).await.unwrap_err();
  assert!(error.to_string().contains("not accepted"));
  assert_eq!(enrollment(state.path()).unwrap().1.labels.get("os").map(String::as_str), Some("darwin"));
  assert!(!enrollment(state.path()).unwrap().1.labels.contains_key("invalid key"));
}

#[tokio::test]
async fn preprovisioned_credential_starts_readonly_without_writing_runner_json() {
  let served = control(Duration::from_secs(15)).await;
  let minted = operator(&served)
    .runners()
    .mint_runner_credential(needy_api_client::proto::v1::MintRunnerCredentialRequest {
      name: "k8s-exec".into(),
      ttl_ms: 60_000,
      ..Default::default()
    })
    .await
    .unwrap()
    .into_inner();
  assert_eq!(credential_name(&minted.credential), Some("k8s-exec"));

  let state = tempfile::tempdir().unwrap();
  let mut cfg = config(&served, state.path(), None);
  cfg.name = credential_name(&minted.credential).unwrap().to_string();
  cfg.credential = Some(minted.credential);

  let mut runner = Runner::start(cfg).await.unwrap();
  runner.beat().await.unwrap();

  assert!(!state.path().join("runner.json").exists(), "read-only mode must never write runner.json");
  let runners = listed(&served).await;
  assert_eq!(runners.len(), 1);
  assert_eq!((runners[0].name.as_str(), runners[0].status()), ("k8s-exec", RunnerStatus::Online));
}

#[tokio::test]
async fn activity_hook_is_throttled_and_nonzero_exit_does_not_abort_exec() {
  use needy_api_server::control::{ExecOpen, ExecOutput, ExecSpec, Processes};

  let served = control(Duration::from_secs(15)).await;
  let minted = operator(&served)
    .runners()
    .mint_runner_credential(needy_api_client::proto::v1::MintRunnerCredentialRequest {
      name: "hooked".into(),
      ttl_ms: 60_000,
      ..Default::default()
    })
    .await
    .unwrap()
    .into_inner();
  let state = tempfile::tempdir().unwrap();
  let log = state.path().join("hook.log");
  let hook_cmd = format!("echo tick >> '{}' && exit 7", log.display());
  let mut cfg = config(&served, state.path(), None);
  cfg.name = "hooked".into();
  cfg.credential = Some(minted.credential);
  cfg.activity_hook = Some(ActivityHookConfig {
    command: hook_cmd,
    interval: Duration::from_secs(60),
  });
  let runner = Runner::start(cfg).await.unwrap();
  let stop = CancellationToken::new();
  let _serving = tokio::spawn(runner.serve(stop.clone()));
  eventually("runner connects", async || served.daemon().links.get("hooked").is_some()).await;

  for expected in ["first", "second"] {
    let spec = ExecSpec {
      argv: vec!["sh".into(), "-c".into(), format!("echo {expected}")],
      cwd: String::new(),
      env: Default::default(),
      clear_env: false,
      tty: None,
      detach: false,
      run: String::new(),
      executor: String::new(),
      runner: "hooked".into(),
      no_stdin: true,
      origin: needy_core::types::Origin::Cli,
      search_path_if_missing: false,
      by: None,
    };
    let mut outputs = served.daemon().exec(ExecOpen::Start(spec), Box::pin(tokio_stream::empty())).await.unwrap();
    let mut stdout = Vec::new();
    let mut exit_code = None;
    while let Some(item) = outputs.next().await {
      match item.unwrap() {
        ExecOutput::Stdout(bytes) => stdout.extend_from_slice(&bytes),
        ExecOutput::Exited(exit) => exit_code = exit.code,
        _ => {}
      }
    }
    assert_eq!(exit_code, Some(0), "non-zero activity hook must not fail exec");
    assert_eq!(String::from_utf8_lossy(&stdout).trim(), expected);
  }

  eventually("hook wrote tick", async || std::fs::read_to_string(&log).is_ok_and(|s| !s.is_empty())).await;
  tokio::time::sleep(Duration::from_millis(100)).await;
  let ticks: Vec<_> = std::fs::read_to_string(&log).unwrap().lines().map(str::to_string).collect();
  assert_eq!(ticks.len(), 1, "hook must be throttled within interval, got {ticks:?}");
  stop.cancel();
}
