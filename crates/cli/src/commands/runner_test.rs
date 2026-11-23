use needy_api_client::proto::v1;
use needy_core::types::RunnerRole;

use super::*;

#[test]
fn joining_guidance_names_the_control_host_command_and_script_inputs() {
  let text = joining_guidance(Some(&[RunnerRole::Executor]));

  assert!(text.contains("run on the control host:"), "{text}");
  assert!(text.contains("needy runner invite --ttl 1h --roles executor"), "{text}");
  assert!(text.contains("--code-file FILE"), "{text}");
  assert!(text.contains("--code-file -"), "{text}");
}

#[test]
fn roles_statuses_and_token_states_are_read_from_the_wire() {
  assert_eq!(role(v1::RunnerRole::Actor as i32).unwrap(), RunnerRole::Actor);
  assert_eq!(role(v1::RunnerRole::Executor as i32).unwrap(), RunnerRole::Executor);
  assert_eq!(runner_status(v1::RunnerStatus::Offline as i32).unwrap(), "offline");
  assert_eq!(token_state(v1::JoinTokenState::UsedUp as i32).unwrap(), "used-up");
}

#[test]
fn unspecified_and_unknown_wire_values_are_refused() {
  assert_eq!(role(v1::RunnerRole::Unspecified as i32).unwrap_err().to_string(), "the runner role is unspecified");
  assert_eq!(runner_status(42).unwrap_err().to_string(), "unknown runner status 42");
  assert_eq!(token_state(0).unwrap_err().to_string(), "the join token state is unspecified");
}

#[test]
fn a_label_is_split_at_its_first_equals_sign() {
  assert_eq!(label("os=darwin").unwrap(), ("os".to_string(), "darwin".to_string()));
  assert_eq!(label("k=a=b").unwrap(), ("k".to_string(), "a=b".to_string()));
  assert_eq!(label("=x").unwrap_err(), "a label is KEY=VALUE, not \"=x\"");
}

fn kvm_runner() -> v1::Runner {
  v1::Runner {
    name: "kvm".into(),
    status: v1::RunnerStatus::Online as i32,
    capabilities: Some(v1::RunnerCapabilities {
      actor: vec![v1::ActorDriver::LinuxFirecracker as i32],
      executor: vec![
        v1::ExecutorDriver::Native as i32,
        v1::ExecutorDriver::LinuxContainer as i32,
      ],
    }),
    host: Some(v1::RunnerHost {
      os: "linux".into(),
      arch: "x86_64".into(),
      cpu_vendor: "GenuineIntel".into(),
      cpu_model: "Example CPU".into(),
      kvm_cpuid: Some(v1::KvmCpuid::default()),
      ..Default::default()
    }),
    labels: [("kvm".to_string(), String::new())].into(),
    last_seen_at_ms: needy_common::time::now_ms(),
    ..Default::default()
  }
}

#[test]
fn runner_ls_shows_capabilities_and_the_host_in_the_table() {
  let row = RunnerRow::try_from(kvm_runner()).unwrap();

  let cells = row.cells();

  assert_eq!(
    cells[..5],
    [
      "kvm",
      "online",
      "actor(linux-firecracker) executor(linux-container,native)",
      "linux/x86_64",
      "Example CPU"
    ]
  );
}

#[test]
fn runner_ls_shows_capabilities_by_role_and_the_host_in_json() {
  let row = RunnerRow::try_from(kvm_runner()).unwrap();

  let json = serde_json::to_value(&row).unwrap();

  assert_eq!(json["capabilities"], serde_json::json!({"actor": ["linux-firecracker"], "executor": ["native", "linux-container"]}));
  assert_eq!(
    json["host"],
    serde_json::json!({"os": "linux", "arch": "x86_64", "cpu_vendor": "GenuineIntel", "cpu_model": "Example CPU", "kvm": true})
  );
}

#[test]
fn a_runner_without_a_host_report_shows_dashes() {
  let runner = v1::Runner {
    host: None,
    capabilities: Some(v1::RunnerCapabilities {
      executor: vec![v1::ExecutorDriver::Native as i32],
      ..Default::default()
    }),
    ..kvm_runner()
  };

  let cells = RunnerRow::try_from(runner).unwrap().cells();

  assert_eq!(cells[2..5], ["executor(native)", "-", "-"]);
}

#[test]
fn an_unknown_provider_from_the_wire_is_refused() {
  let reported = v1::RunnerCapabilities {
    executor: vec![42],
    ..Default::default()
  };

  assert_eq!(capabilities(Some(reported)).unwrap_err().to_string(), "unknown executor driver 42");
}

#[test]
fn runner_serve_accepts_readonly_credential_flags() {
  use clap::Parser as _;

  #[derive(clap::Parser)]
  struct Wrapper {
    #[command(flatten)]
    args: Args,
  }

  let parsed = Wrapper::try_parse_from([
    "needy",
    "serve",
    "--address",
    "tcp://192.0.2.10:7443",
    "--credential-env",
    "NEEDY_RUNNER_CRED",
  ])
  .unwrap();

  match parsed.args.command {
    Command::Serve(serve) => {
      assert_eq!(serve.address.as_deref(), Some("tcp://192.0.2.10:7443"));
      assert_eq!(serve.name, None);
      assert_eq!(serve.credential_env.as_deref(), Some("NEEDY_RUNNER_CRED"));
      assert_eq!(serve.credential_file, None);
    }
    _ => panic!("expected serve subcommand"),
  }

  // --credential-env and --credential-file conflict.
  assert!(
    Wrapper::try_parse_from([
      "needy",
      "serve",
      "--address",
      "tcp://192.0.2.10:7443",
      "--credential-env",
      "A",
      "--credential-file",
      "/tmp/cred",
    ])
    .is_err()
  );
}
