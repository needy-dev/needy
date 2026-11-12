use std::collections::BTreeMap;

use needy_config::profile::{ActorDriver, ExecutorDriver};
use needy_config::profile::{LabelMatch, RunnerSelector};

use super::{Candidate, Need, pick};
use crate::testing::capabilities;

const KVM: &str = "actor(linux-firecracker) executor(linux-container,native)";
const NATIVE: &str = "executor(native)";

fn runner(name: &str, has: &str, labels: &[(&str, &str)], connected: bool) -> Candidate {
  Candidate {
    name: name.into(),
    capabilities: capabilities(has),
    labels: labels.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<BTreeMap<_, _>>(),
    connected,
  }
}

fn labels(texts: &[&str]) -> RunnerSelector {
  RunnerSelector::Labels(texts.iter().map(|t| t.parse::<LabelMatch>().unwrap()).collect())
}

fn name(text: &str) -> RunnerSelector {
  RunnerSelector::Name(text.into())
}

const ACTOR: Need = Need::Actor(ActorDriver::LinuxFirecracker);
const NATIVE_EXECUTOR: Need = Need::Executor(ExecutorDriver::Native);
const CONTAINER_EXECUTOR: Need = Need::Executor(ExecutorDriver::LinuxContainer);

fn fleet() -> Vec<Candidate> {
  vec![
    runner("kvm-1", KVM, &[("kvm", ""), ("os", "linux")], true),
    runner("kvm-2", KVM, &[("kvm", ""), ("os", "linux")], false),
    runner("mac", NATIVE, &[("os", "darwin")], true),
    runner("gpu", "executor(linux-container,native)", &[("os", "linux"), ("gpu", "a100")], true),
  ]
}

#[test]
fn a_name_picks_that_runner() {
  assert_eq!(pick(&name("mac"), NATIVE_EXECUTOR, &fleet()).unwrap(), "mac");
}

#[test]
fn a_name_nobody_joined_as_is_an_error_naming_the_runners() {
  let err = pick(&name("windows"), NATIVE_EXECUTOR, &fleet()).unwrap_err().to_string();

  assert_eq!(err, "no runner is named windows; the runners are gpu, kvm-1, kvm-2, mac");
}

#[test]
fn a_named_runner_without_the_role_is_an_error_naming_the_runners_that_have_it() {
  let err = pick(&name("mac"), ACTOR, &fleet()).unwrap_err().to_string();

  assert_eq!(
    err,
    "runner mac cannot host actor(linux-firecracker): it has executor(native); runners with actor(linux-firecracker): kvm-1, kvm-2"
  );
}

#[test]
fn a_docker_executor_on_a_runner_without_docker_is_an_error_naming_the_runners_with_docker() {
  let err = pick(&name("mac"), CONTAINER_EXECUTOR, &fleet()).unwrap_err().to_string();

  assert_eq!(
    err,
    "runner mac cannot host executor(linux-container): it has executor(native); runners with executor(linux-container): gpu, kvm-1, kvm-2"
  );
}

#[test]
fn a_capability_no_runner_has_says_so() {
  let err = pick(&name("mac"), CONTAINER_EXECUTOR, &[runner("mac", NATIVE, &[], true)]).unwrap_err().to_string();

  assert_eq!(err, "runner mac cannot host executor(linux-container): it has executor(native); no runner has executor(linux-container)");
}

#[test]
fn a_runner_with_only_the_container_runtime_hosts_no_firecracker_actor() {
  let gvisor = [runner(
    "gvisor",
    "actor(linux-container) executor(linux-container,native)",
    &[],
    true,
  )];

  let err = pick(&name("gvisor"), ACTOR, &gvisor).unwrap_err().to_string();

  assert_eq!(
    err,
    "runner gvisor cannot host actor(linux-firecracker): it has actor(linux-container) executor(linux-container,native); no runner has actor(linux-firecracker)"
  );
}

#[test]
fn a_container_actor_is_placed_on_a_runner_with_the_container_runtime() {
  let fleet = [
    runner("gvisor", "actor(linux-container) executor(linux-container,native)", &[("cloud", "")], true),
    runner("kvm", KVM, &[("cloud", "")], true),
  ];

  let picked = pick(&labels(&["cloud"]), Need::Actor(ActorDriver::LinuxContainer), &fleet).unwrap();

  assert_eq!(picked, "gvisor");
}

#[test]
fn a_named_runner_that_is_not_connected_is_an_error() {
  let err = pick(&name("kvm-2"), ACTOR, &fleet()).unwrap_err().to_string();

  assert_eq!(err, "runner kvm-2 is not connected");
}

#[test]
fn labels_pick_the_one_connected_runner_with_the_capability_and_every_label() {
  // kvm-2 carries the labels too but is not connected.
  assert_eq!(pick(&labels(&["kvm"]), ACTOR, &fleet()).unwrap(), "kvm-1");
  assert_eq!(pick(&labels(&["os=darwin"]), NATIVE_EXECUTOR, &fleet()).unwrap(), "mac");
  assert_eq!(pick(&labels(&["os=linux", "gpu"]), NATIVE_EXECUTOR, &fleet()).unwrap(), "gpu");
}

#[test]
fn labels_that_fit_several_connected_runners_are_ambiguous() {
  let err = pick(&labels(&["os=linux"]), NATIVE_EXECUTOR, &fleet()).unwrap_err().to_string();

  assert_eq!(
    err,
    "labels os=linux fit 2 connected runners with executor(native): gpu, kvm-1; \
     name one with `runner: NAME` or add a label that tells them apart"
  );
}

#[test]
fn labels_no_connected_runner_fits_are_an_error_naming_the_candidates() {
  let err = pick(
    &labels(&["kvm"]),
    NATIVE_EXECUTOR,
    &[
      runner("kvm-2", KVM, &[("kvm", "")], false),
      runner("mac", NATIVE, &[], true),
    ],
  )
  .unwrap_err()
  .to_string();

  assert_eq!(err, "no connected runner with executor(native) has labels kvm; candidates: kvm-2 (not connected), mac (no label kvm)");
}

#[test]
fn labels_name_the_capabilities_of_runners_without_the_provider() {
  let err = pick(&labels(&["os=darwin"]), CONTAINER_EXECUTOR, &fleet()).unwrap_err().to_string();

  assert_eq!(
    err,
    "no connected runner with executor(linux-container) has labels os=darwin; candidates: gpu (no label os=darwin), \
     kvm-1 (no label os=darwin), kvm-2 (no label os=darwin), mac (has executor(native))"
  );
}

#[test]
fn labels_without_any_runner_say_none_joined() {
  let err = pick(&labels(&["kvm"]), ACTOR, &[]).unwrap_err().to_string();

  assert_eq!(err, "no connected runner with actor(linux-firecracker) has labels kvm; no runner has joined");
}

#[test]
fn a_runner_without_the_role_says_so() {
  let err = pick(&labels(&["os=darwin"]), ACTOR, &fleet()).unwrap_err().to_string();

  assert!(err.ends_with("mac (no actor role)"), "{err}");
}

#[test]
fn the_choice_does_not_depend_on_the_order_of_the_runners() {
  let mut reversed = fleet();
  reversed.reverse();

  assert_eq!(pick(&labels(&["kvm"]), ACTOR, &reversed).unwrap(), "kvm-1");
  let err = pick(&labels(&["os=linux"]), NATIVE_EXECUTOR, &reversed).unwrap_err().to_string();
  assert!(err.contains("gpu, kvm-1;"), "{err}");
}
