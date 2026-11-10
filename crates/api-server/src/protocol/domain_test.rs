use needy_api_proto::v1;
use needy_core::types::Origin;

use super::*;

#[test]
fn every_origin_survives_the_wire() {
  for value in [
    Origin::Cli,
    Origin::Fs,
    Origin::Files,
    Origin::ActorProxy,
    Origin::ActorMount,
  ] {
    assert_eq!(origin(proto_origin(value) as i32).unwrap(), value);
  }
}

#[test]
fn an_unspecified_origin_is_refused() {
  let error = origin(v1::Origin::Unspecified as i32).unwrap_err();

  assert_eq!(error.to_string(), "the exec origin is unspecified");
}

#[test]
fn an_unknown_origin_is_refused() {
  let error = origin(42).unwrap_err();

  assert_eq!(error.to_string(), "unknown exec origin 42");
}

fn heartbeat_with(host: &RunnerHost) -> v1::HeartbeatRequest {
  v1::HeartbeatRequest {
    roles: vec![v1::RunnerRole::Actor as i32],
    labels: Default::default(),
    host: Some(proto_runner_host(host)),
  }
}

#[test]
fn a_runners_host_survives_the_wire_with_its_kvm_cpuid() {
  let host = RunnerHost {
    os: "linux".into(),
    arch: "x86_64".into(),
    cpu: needy_core::recipe::HostInfo {
      flags: ["avx2".to_string(), "sse4_2".to_string()].into(),
      vendor: "GenuineIntel".into(),
      model_name: "Example CPU".into(),
      kvm_cpuid: Some([(0x7, 0), (0xd, 1)].into()),
    },
  };

  let report = runner_report(heartbeat_with(&host)).unwrap();

  assert_eq!(report.host, Some(host));
}

// A host without KVM reports no CPUID entries, which is not the same as KVM
// supporting none: the recipe's KVM checks are skipped, not failed.
#[test]
fn a_host_without_kvm_keeps_its_cpuid_unknown() {
  let host = RunnerHost {
    os: "macos".into(),
    arch: "aarch64".into(),
    cpu: Default::default(),
  };

  let report = runner_report(heartbeat_with(&host)).unwrap();

  assert_eq!(report.host.unwrap().cpu.kvm_cpuid, None);
}

#[test]
fn a_heartbeat_without_a_host_reports_none() {
  let request = v1::HeartbeatRequest {
    roles: vec![v1::RunnerRole::Executor as i32],
    ..Default::default()
  };

  assert_eq!(runner_report(request).unwrap().host, None);
}
