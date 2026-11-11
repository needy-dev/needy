//! `profile check` through an in-process control plane, with runners that
//! joined and connected, and the host each one reports.

use needy_api_client::Client;
use needy_api_server::control::{ControlError, Placement, ProfileCheck, ProfileToCheck, RunnerHost, RunnerReport, Runners, Runs};

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

async fn runner(served: &Served, name: &str, capabilities: &str, labels: &[(&str, &str)]) -> JoinedRunner {
  testing::join_runner(served, &operator(served), name, capabilities, labels).await
}

/// Reports `host` for `runner`, as its heartbeat does; a heartbeat reports
/// the capabilities and labels whole.
async fn report_host(served: &Served, runner: &str, capabilities: &str, labels: &[(&str, &str)], host: RunnerHost) {
  let report = RunnerReport {
    capabilities: testing::capabilities(capabilities),
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
  let _kvm = runner(&served, "kvm", "actor(firecracker) executor(docker,native)", &[("kvm", "")]).await;
  report_host(&served, "kvm", "actor(firecracker) executor(docker,native)", &[("kvm", "")], host_without_avx512()).await;

  let check = check(&served, "actor: { runtime: firecracker, runner: { labels: [kvm] }, recipe: \"builtin:gce-ubuntu-22.04\" }").await;

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
  let _kvm = runner(&served, "kvm", "actor(firecracker) executor(docker,native)", &[("kvm", "")]).await;
  let _mac = runner(&served, "mac", "executor(native)", &[("os", "darwin")]).await;

  let check = check(
    &served,
    r#"
actor: { runtime: firecracker, runner: kvm }
executors:
  mac: { runner: { labels: [os=darwin] }, path_map: { /home/agent: /Users/user/work } }
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
  let _mac = runner(&served, "mac", "executor(native)", &[("os", "darwin")]).await;

  let check = check(
    &served,
    "actor: { runtime: firecracker }\nexecutors:\n  linux: { runner: { labels: [os=linux] } }\n  gone: { runner: windows }\n",
  )
  .await;

  assert!(!check.executors.contains_key("linux"));
  assert_eq!(
    messages(&check),
    [
      "error: executors.gone: no runner is named windows; the runners are mac",
      "error: executors.linux: no connected runner with executor(native) has labels os=linux; candidates: mac (no label os=linux)",
    ]
  );
}

#[tokio::test]
async fn an_actor_on_a_runner_other_than_the_control_hosts_own_is_an_error_finding() {
  let served = control().await;
  let _far = runner(&served, "far", "actor(firecracker)", &[]).await;

  let check = check(&served, "actor: { runtime: firecracker, runner: far }").await;

  assert_eq!(check.actor, Some(Placement::Runner("far".into())));
  let found = messages(&check);
  assert_eq!(found.len(), 1, "{found:?}");
  assert!(found[0].starts_with("error: actor: runner far cannot host actors yet"), "{found:?}");
}

#[tokio::test]
async fn docker_on_a_runner_other_than_the_control_hosts_own_is_an_error_finding() {
  let served = control().await;
  let _mac = runner(&served, "mac", "executor(docker,native)", &[]).await;

  let check = check(&served, "actor: { runtime: firecracker }\nexecutors: { build: { runner: mac, provider: docker } }").await;

  let found = messages(&check);
  assert_eq!(found.len(), 1, "{found:?}");
  assert!(found[0].starts_with("error: executors.build: runner mac cannot create Docker executors yet"), "{found:?}");
}

#[tokio::test]
async fn docker_on_a_runner_without_docker_is_an_error_finding_naming_the_runners_with_docker() {
  let served = control().await;
  let _kvm = runner(&served, "kvm", "actor(firecracker) executor(docker,native)", &[]).await;
  let _mac = runner(&served, "mac", "executor(native)", &[]).await;

  let check = check(&served, "actor: { runtime: firecracker }\nexecutors: { build: { runner: mac, provider: docker } }").await;

  assert!(!check.executors.contains_key("build"));
  assert_eq!(
    messages(&check),
    ["error: executors.build: runner mac cannot host executor(docker): it has executor(native); runners with executor(docker): kvm"]
  );
}

#[tokio::test]
async fn a_runner_that_reported_no_host_skips_the_recipe_checks_with_a_warning() {
  let served = control().await;
  let _kvm = runner(&served, "kvm", "actor(firecracker)", &[]).await;
  // As after a restart of the control plane, before the next heartbeat.
  served.daemon().runner_hosts.lock().unwrap().clear();

  let check = check(&served, "actor: { runtime: firecracker, runner: kvm, recipe: \"builtin:gce-ubuntu-22.04\" }").await;

  assert_eq!(check.actor_cpu, "");
  assert_eq!(messages(&check), ["warning: runner kvm has not reported its host yet; the recipe's host checks are skipped"]);
}

// ROOT CAUSE:
//
// If a native executor on a runner other than the control host's own had
// no `path_map` for the actor's working directory, every command routed to
// it kept the actor's directory (`/home/agent`), which that machine does not
// have, and failed to start, while `profile check` found nothing. A
// `workspace` does not help: it is the directory of a command that names
// none, and a routed command names the actor's.
//
// Before the fix, the check said nothing about it.
// The fix warns when no `path_map` entry covers the actor's directory there.
#[tokio::test]
async fn a_native_executor_on_another_runner_without_a_path_map_for_the_actors_directory_is_a_warning() {
  let served = control().await;
  let _kvm = runner(&served, "kvm", "actor(firecracker) executor(docker,native)", &[]).await;
  let _mac = runner(&served, "mac", "executor(native)", &[]).await;

  let check = check(
    &served,
    r#"
actor: { runtime: firecracker }
executors:
  mac: { runner: mac, workspace: /Users/user/work }
  mapped: { runner: mac, path_map: { /home/agent: /Users/user/work } }
  own: { runner: kvm }
  here: {}
"#,
  )
  .await;

  assert_eq!(
    messages(&check),
    [
      "warning: executors.mac: commands routed to runner mac keep the actor's working directory /home/agent, which \
       no path_map entry maps to a directory on that machine; add one, for example `path_map: { /home/agent: /path/on/mac }`"
    ]
  );
}

#[tokio::test]
async fn a_path_map_that_covers_the_actors_cwd_satisfies_the_check() {
  let served = control().await;
  let _mac = runner(&served, "mac", "executor(native)", &[]).await;

  let check = check(
    &served,
    "actor: { runtime: firecracker, cwd: /srv/project }\nexecutors:\n  mac: { runner: mac, path_map: { /srv: /Users/user/srv } }\n",
  )
  .await;

  assert_eq!(messages(&check), Vec::<String>::new());
}

// ROOT CAUSE:
//
// If a `path_map` key started with `~`, the check compared it as written
// against the actor's expanded working directory, so it warned about a
// directory the entry was meant to cover; routing did not expand it either.
//
// Before the fix, `{ ~/workspace: ... }` and `{ ~: ... }` covered nothing.
// The fix expands a leading `~` of every `path_map` key to the profile
// user's home when the profile loads, so the check and routing read the
// same key.
#[tokio::test]
async fn a_path_map_key_under_the_users_home_covers_the_actors_cwd() {
  let served = control().await;
  let _mac = runner(&served, "mac", "executor(native)", &[]).await;

  let check = check(
    &served,
    r#"
actor: { runtime: firecracker, cwd: ~/workspace/project }
user: { name: dev, uid: 1000, gid: 1000 }
executors:
  workspace: { runner: mac, path_map: { "~/workspace": /Users/user/work } }
  home: { runner: mac, path_map: { "~": /Users/user } }
  other: { runner: mac, path_map: { "~/elsewhere": /Users/user/elsewhere } }
"#,
  )
  .await;

  assert_eq!(
    messages(&check),
    [
      "warning: executors.other: commands routed to runner mac keep the actor's working directory /home/dev/workspace/project, \
       which no path_map entry maps to a directory on that machine; add one, for example \
       `path_map: { /home/dev/workspace/project: /path/on/mac }`"
    ]
  );
}

// ROOT CAUSE:
//
// If the actor's host had no usable KVM, `profile check` reported nothing,
// because only `run create` probed the KVM device.
//
// Before the fix, a profile checked clean and its run then failed at create.
// The fix makes a missing KVM on the actor's host an error finding that names
// the host, as the create's refusal does.
#[tokio::test]
async fn a_control_host_without_kvm_is_an_error_finding_naming_it() {
  let served = control().await;
  let kvm = served.dir().join("kvm");
  std::fs::remove_file(&kvm).unwrap();

  let check = check(&served, "actor: { runtime: firecracker, command: [sh] }").await;

  let found = messages(&check);
  assert_eq!(found.len(), 1, "{found:?}");
  assert!(
    found[0].starts_with(&format!("error: actor: on the control host: this host has no usable KVM: cannot open {}", kvm.display())),
    "{found:?}"
  );
}

#[tokio::test]
async fn the_control_hosts_own_runner_without_kvm_is_an_error_finding_naming_it() {
  let served = control().await;
  let _kvm = runner(&served, "kvm", "actor(firecracker)", &[]).await;
  std::fs::remove_file(served.dir().join("kvm")).unwrap();

  let check = check(&served, "actor: { runtime: firecracker, runner: kvm }").await;

  let found = messages(&check);
  assert_eq!(found.len(), 1, "{found:?}");
  assert!(found[0].starts_with("error: actor: on runner kvm (the control host): this host has no usable KVM"), "{found:?}");
}

// Every run has an actor (owner decision, 2026-11-11), so `actor:` with its
// `runtime` is required and a profile without one does not load.
#[tokio::test]
async fn a_profile_without_an_actor_is_refused_for_its_missing_runtime() {
  let served = control().await;
  std::fs::remove_file(served.dir().join("kvm")).unwrap();
  let request = ProfileToCheck {
    profile_yaml: "executors: { here: {} }".into(),
    recipe_yaml: String::new(),
  };

  let Err(ControlError::Invalid(message)) = Runs::check(served.daemon(), request).await else {
    panic!("a profile without an actor must be refused");
  };

  assert!(message.contains("missing field `actor`"), "{message}");
}

#[tokio::test]
async fn a_container_actor_without_its_options_is_refused_and_not_checked_for_kvm() {
  let served = control().await;
  std::fs::remove_file(served.dir().join("kvm")).unwrap();

  let check = check(&served, "actor: { runtime: container }").await;

  assert_eq!(messages(&check), ["error: actor.container: a container actor needs `container: { oci_runtime: runc }` (or `runsc`, gVisor)"]);
}
