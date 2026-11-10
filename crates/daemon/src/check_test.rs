//! `profile check` through an in-process control plane, with runners that
//! joined and connected, and the host each one reports.

use needy_api_client::Client;
use needy_api_server::control::{Placement, ProfileCheck, ProfileToCheck, RunnerHost, RunnerReport, Runners, Runs};
use needy_core::types::RunnerRole::{self, Actor, Executor};

use crate::testing::{self, JoinedRunner, Served};

const CONTROL_TOKEN: &str = "control-secret";

/// A control plane whose own runner is `kvm`.
async fn control() -> Served {
  let started = testing::daemon_with(|config| config.local_runner = Some("kvm".into())).await;
  testing::serve(started, Some(CONTROL_TOKEN)).await
}

fn operator(served: &Served) -> Client {
  served.client.clone().with_token(CONTROL_TOKEN).unwrap()
}

async fn runner(served: &Served, name: &str, roles: &[RunnerRole], labels: &[(&str, &str)]) -> JoinedRunner {
  testing::join_runner(served, &operator(served), name, roles, labels).await
}

/// Reports `host` for `runner`, as its heartbeat does; a heartbeat reports
/// the roles and labels whole.
async fn report_host(served: &Served, runner: &str, roles: &[RunnerRole], labels: &[(&str, &str)], host: RunnerHost) {
  let report = RunnerReport {
    roles: roles.iter().copied().collect(),
    labels: labels.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
    host: Some(host),
  };
  Runners::heartbeat(served.daemon(), runner, report).await.unwrap();
}

/// A Linux host whose CPU has AVX2 but no AVX-512.
fn host_without_avx512() -> RunnerHost {
  RunnerHost {
    os: "linux".into(),
    arch: "x86_64".into(),
    cpu: needy_core::recipe::HostInfo {
      flags: ["avx2".to_string(), "bmi2".to_string(), "adx".to_string()].into(),
      vendor: "GenuineIntel".into(),
      model_name: "Example CPU without AVX-512".into(),
      kvm_cpuid: None,
    },
  }
}

async fn check(served: &Served, profile: &str) -> ProfileCheck {
  let request = ProfileToCheck {
    profile_yaml: profile.into(),
    recipe_yaml: String::new(),
  };
  Runs::check(served.daemon(), request).await.unwrap()
}

fn messages(check: &ProfileCheck) -> Vec<String> {
  check.findings.iter().map(|f| format!("{}: {}", if f.error { "error" } else { "warning" }, f.message)).collect()
}

#[tokio::test]
async fn the_recipe_is_checked_against_the_cpu_of_the_runner_the_actor_is_scheduled_on() {
  let served = control().await;
  let _kvm = runner(&served, "kvm", &[Actor, Executor], &[("kvm", "")]).await;
  report_host(&served, "kvm", &[Actor, Executor], &[("kvm", "")], host_without_avx512()).await;

  let check = check(&served, "actor: { runner: { labels: [kvm] }, recipe: \"builtin:gce-ubuntu-22.04\" }").await;

  assert_eq!(check.actor, Some(Placement::Runner("kvm".into())));
  assert_eq!(check.actor_cpu, "Example CPU without AVX-512");
  let found = messages(&check);
  assert!(
    found
      .iter()
      .any(|m| m.starts_with("error: recipe: ") && m.contains("needs avx512f, but the host CPU (Example CPU without AVX-512) lacks it")),
    "{found:?}"
  );
}

#[tokio::test]
async fn executor_selectors_resolve_to_runners_or_the_control_host() {
  let served = control().await;
  let _kvm = runner(&served, "kvm", &[Actor, Executor], &[("kvm", "")]).await;
  let _mac = runner(&served, "mac", &[Executor], &[("os", "darwin")]).await;

  let check = check(
    &served,
    r#"
actor: { runner: kvm }
executors:
  mac: { runner: { labels: [os=darwin] } }
  build: { runner: kvm, provider: docker }
  here: {}
"#,
  )
  .await;

  assert_eq!(check.actor, Some(Placement::Runner("kvm".into())));
  assert_eq!(check.executors["mac"], Placement::Runner("mac".into()));
  assert_eq!(check.executors["build"], Placement::Runner("kvm".into()));
  assert_eq!(check.executors["here"], Placement::ControlHost);
  assert_eq!(messages(&check), Vec::<String>::new());
}

#[tokio::test]
async fn a_selector_that_names_no_connected_runner_is_an_error_finding() {
  let served = control().await;
  let _mac = runner(&served, "mac", &[Executor], &[("os", "darwin")]).await;

  let check = check(&served, "executors:\n  linux: { runner: { labels: [os=linux] } }\n  gone: { runner: windows }\n").await;

  assert!(!check.executors.contains_key("linux"));
  assert_eq!(
    messages(&check),
    [
      "error: executors.gone: no runner is named windows; the runners are mac",
      "error: executors.linux: no connected runner with the executor role has labels os=linux; candidates: mac (no label os=linux)",
    ]
  );
}

#[tokio::test]
async fn an_actor_on_a_runner_other_than_the_control_hosts_own_is_an_error_finding() {
  let served = control().await;
  let _far = runner(&served, "far", &[Actor], &[]).await;

  let check = check(&served, "actor: { runner: far }").await;

  assert_eq!(check.actor, Some(Placement::Runner("far".into())));
  let found = messages(&check);
  assert_eq!(found.len(), 1, "{found:?}");
  assert!(found[0].starts_with("error: actor: runner far cannot host actors yet"), "{found:?}");
}

#[tokio::test]
async fn docker_on_a_runner_other_than_the_control_hosts_own_is_an_error_finding() {
  let served = control().await;
  let _mac = runner(&served, "mac", &[Executor], &[]).await;

  let check = check(&served, "executors: { build: { runner: mac, provider: docker } }").await;

  let found = messages(&check);
  assert_eq!(found.len(), 1, "{found:?}");
  assert!(found[0].starts_with("error: executors.build: runner mac cannot create Docker executors yet"), "{found:?}");
}

#[tokio::test]
async fn a_runner_that_reported_no_host_skips_the_recipe_checks_with_a_warning() {
  let served = control().await;
  let _kvm = runner(&served, "kvm", &[Actor], &[]).await;
  // As after a restart of the control plane, before the next heartbeat.
  served.daemon().runner_hosts.lock().unwrap().clear();

  let check = check(&served, "actor: { runner: kvm, recipe: \"builtin:gce-ubuntu-22.04\" }").await;

  assert_eq!(check.actor_cpu, "");
  assert_eq!(messages(&check), ["warning: runner kvm has not reported its host yet; the recipe's host checks are skipped"]);
}
