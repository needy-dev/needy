use std::collections::{BTreeMap, BTreeSet};

use needy_config::profile::{LabelMatch, RunnerSelector};
use needy_core::types::RunnerRole;

use super::{Candidate, pick};

fn runner(name: &str, roles: &[RunnerRole], labels: &[(&str, &str)], connected: bool) -> Candidate {
  Candidate {
    name: name.into(),
    roles: roles.iter().copied().collect::<BTreeSet<_>>(),
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

use RunnerRole::{Actor, Executor};

fn fleet() -> Vec<Candidate> {
  vec![
    runner("kvm-1", &[Actor, Executor], &[("kvm", ""), ("os", "linux")], true),
    runner("kvm-2", &[Actor, Executor], &[("kvm", ""), ("os", "linux")], false),
    runner("mac", &[Executor], &[("os", "darwin")], true),
    runner("gpu", &[Executor], &[("os", "linux"), ("gpu", "a100")], true),
  ]
}

#[test]
fn a_name_picks_that_runner() {
  assert_eq!(pick(&name("mac"), Executor, &fleet()).unwrap(), "mac");
}

#[test]
fn a_name_nobody_joined_as_is_an_error_naming_the_runners() {
  let err = pick(&name("windows"), Executor, &fleet()).unwrap_err().to_string();

  assert_eq!(err, "no runner is named windows; the runners are gpu, kvm-1, kvm-2, mac");
}

#[test]
fn a_named_runner_without_the_role_is_an_error() {
  let err = pick(&name("mac"), Actor, &fleet()).unwrap_err().to_string();

  assert_eq!(err, "runner mac cannot host an actor: its roles are executor");
}

#[test]
fn a_named_runner_that_is_not_connected_is_an_error() {
  let err = pick(&name("kvm-2"), Actor, &fleet()).unwrap_err().to_string();

  assert_eq!(err, "runner kvm-2 is not connected");
}

#[test]
fn labels_pick_the_one_connected_runner_with_the_role_and_every_label() {
  // kvm-2 carries the labels too but is not connected.
  assert_eq!(pick(&labels(&["kvm"]), Actor, &fleet()).unwrap(), "kvm-1");
  assert_eq!(pick(&labels(&["os=darwin"]), Executor, &fleet()).unwrap(), "mac");
  assert_eq!(pick(&labels(&["os=linux", "gpu"]), Executor, &fleet()).unwrap(), "gpu");
}

#[test]
fn labels_that_fit_several_connected_runners_are_ambiguous() {
  let err = pick(&labels(&["os=linux"]), Executor, &fleet()).unwrap_err().to_string();

  assert_eq!(
    err,
    "labels os=linux fit 2 connected runners with the executor role: gpu, kvm-1; \
     name one with `runner: NAME` or add a label that tells them apart"
  );
}

#[test]
fn labels_no_connected_runner_fits_are_an_error_naming_the_candidates() {
  let err = pick(
    &labels(&["kvm"]),
    Executor,
    &[
      runner("kvm-2", &[Actor, Executor], &[("kvm", "")], false),
      runner("mac", &[Executor], &[], true),
    ],
  )
  .unwrap_err()
  .to_string();

  assert_eq!(err, "no connected runner with the executor role has labels kvm; candidates: kvm-2 (not connected), mac (no label kvm)");
}

#[test]
fn labels_without_any_runner_say_none_joined() {
  let err = pick(&labels(&["kvm"]), Actor, &[]).unwrap_err().to_string();

  assert_eq!(err, "no connected runner with the actor role has labels kvm; no runner has joined");
}

#[test]
fn the_choice_does_not_depend_on_the_order_of_the_runners() {
  let mut reversed = fleet();
  reversed.reverse();

  assert_eq!(pick(&labels(&["kvm"]), Actor, &reversed).unwrap(), "kvm-1");
  let err = pick(&labels(&["os=linux"]), Executor, &reversed).unwrap_err().to_string();
  assert!(err.contains("gpu, kvm-1;"), "{err}");
}
