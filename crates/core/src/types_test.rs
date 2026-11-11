use super::{ActorRuntime, Capabilities, ExecutorProvider, Origin, Principal, RunnerRole};

#[test]
fn an_origins_listed_name_is_its_trace_name() {
  for origin in [
    Origin::Cli,
    Origin::Fs,
    Origin::Files,
    Origin::ActorProxy,
    Origin::ActorMount,
  ] {
    assert_eq!(serde_json::to_value(origin).unwrap(), origin.as_str());
  }
}

#[test]
fn file_transfers_and_mounts_are_needys_own_processes() {
  let internal: Vec<Origin> = [
    Origin::Cli,
    Origin::Fs,
    Origin::Files,
    Origin::ActorProxy,
    Origin::ActorMount,
  ]
  .into_iter()
  .filter(|o| o.is_internal())
  .collect();
  assert_eq!(internal, [Origin::Fs, Origin::Files, Origin::ActorMount]);
}

#[test]
fn a_principal_reads_back_as_it_is_written() {
  for (principal, text) in [
    (Principal::Operator, "operator"),
    (Principal::Client("laptop".into()), "client:laptop"),
    (Principal::Runner("kvm-1".into()), "runner:kvm-1"),
    (Principal::Actor("0a1b2c3d".into()), "actor:0a1b2c3d"),
  ] {
    assert_eq!(principal.to_string(), text);
    assert_eq!(text.parse::<Principal>().unwrap(), principal);
    assert_eq!(serde_json::to_string(&principal).unwrap(), format!("\"{text}\""));
  }
}

#[test]
fn a_principal_of_an_unknown_kind_or_without_a_name_is_refused() {
  for text in ["", "client:", "user:me", "operators"] {
    assert!(text.parse::<Principal>().is_err(), "{text}");
  }
}

fn kvm_host() -> Capabilities {
  Capabilities {
    actor: [ActorRuntime::Firecracker].into(),
    executor: [ExecutorProvider::Native, ExecutorProvider::Docker].into(),
  }
}

#[test]
fn capabilities_are_written_role_by_role_with_their_runtimes_and_providers() {
  assert_eq!(kvm_host().to_string(), "actor(firecracker) executor(docker,native)");
  let mac = Capabilities {
    executor: [ExecutorProvider::Native].into(),
    ..Default::default()
  };
  assert_eq!(mac.to_string(), "executor(native)");
  assert_eq!(Capabilities::default().to_string(), "none");
}

#[test]
fn apple_container_is_spelled_as_a_profile_names_it() {
  let mac = Capabilities {
    actor: [ActorRuntime::AppleContainer].into(),
    executor: [ExecutorProvider::Native].into(),
  };

  assert_eq!(mac.to_string(), "actor(apple-container) executor(native)");
  assert_eq!(serde_json::to_string(&ActorRuntime::AppleContainer).unwrap(), "\"apple-container\"");
}

#[test]
fn capabilities_hold_the_roles_they_name_and_restrict_to_some() {
  assert_eq!(kvm_host().roles(), [RunnerRole::Actor, RunnerRole::Executor].into());

  let executor = kvm_host().within(&[RunnerRole::Executor].into());

  assert_eq!(executor.to_string(), "executor(docker,native)");
  assert_eq!(executor.roles(), [RunnerRole::Executor].into());
  assert!(kvm_host().within(&Default::default()).is_empty());
}

#[test]
fn capabilities_are_stored_as_json_by_role() {
  let json = serde_json::to_value(kvm_host()).unwrap();

  assert_eq!(json, serde_json::json!({"actor": ["firecracker"], "executor": ["native", "docker"]}));
  assert_eq!(serde_json::from_value::<Capabilities>(json).unwrap(), kvm_host());
}
