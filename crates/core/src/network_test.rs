use std::sync::Arc;

use needy_config::profile::ActorNetwork;

use crate::chaos::Injector;
use crate::intercept::net::{Chain, NetContext, NetOutcome};
use crate::types::Network;

use super::*;

/// Allows `*.example.com` and has the service `model`.
fn policy() -> Policy {
  let network: ActorNetwork =
    serde_yaml::from_str("{ allow: [\"*.example.com\"], services: { model: { url: \"http://192.0.2.1:8080\" } } }").unwrap();
  Policy::actor(&network)
}

fn request(host: &str) -> NetContext {
  NetContext {
    network: Network::Actor,
    host: host.into(),
    path: Some("/v1".into()),
  }
}

fn tunnel(host: &str) -> NetContext {
  NetContext {
    path: None,
    ..request(host)
  }
}

#[test]
fn a_service_s_host_in_the_actor_is_its_name_under_needy() {
  let policy = policy();
  assert_eq!(policy.service("model.needy"), Some("http://192.0.2.1:8080"));
  assert_eq!(policy.service("Model.Needy"), Some("http://192.0.2.1:8080"));
  assert_eq!(policy.service("model"), None);
}

#[test]
fn a_host_matching_a_pattern_or_a_service_s_host_is_allowed() {
  let chain = Chain::new(&policy(), None);

  assert_eq!(chain.decide(&request("api.example.com")).outcome, NetOutcome::Allow);
  assert_eq!(chain.decide(&request("example.com")).outcome, NetOutcome::Allow);
  assert_eq!(chain.decide(&request("model.needy")).outcome, NetOutcome::Allow);
  assert_eq!(chain.decide(&tunnel("model.needy")).outcome, NetOutcome::Allow);
  assert_eq!(chain.decide(&request("example.org")).outcome, NetOutcome::Deny);
  assert_eq!(chain.decide(&request("other.needy")).outcome, NetOutcome::Deny);
}

#[test]
fn chaos_follows_the_allowlist() {
  let chaos = serde_yaml::from_str("faults: { busy: { net: { request: model.needy }, status: 503 } }").unwrap();
  let chain = Chain::new(&policy(), Some(Arc::new(Injector::new(&chaos))));

  let decision = chain.decide(&request("model.needy"));

  assert_eq!(decision.outcome, NetOutcome::Fault("busy".into()));
  assert_eq!(decision.fault().and_then(|f| f.effect.status), Some(503));
  assert_eq!(chain.decide(&request("example.org")).outcome, NetOutcome::Deny);
}
