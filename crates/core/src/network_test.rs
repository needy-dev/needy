use std::sync::Arc;

use crate::chaos::Injector;
use crate::intercept::net::{Chain, NetContext, NetOutcome};
use crate::types::Network;

use super::*;

/// Allows `*.example.com`, maps `model.needy` to an upstream and injects a
/// header for it.
fn policy() -> Policy {
  Policy {
    allow: vec![Glob::new("*.example.com")],
    upstream: BTreeMap::from([("model.needy".to_string(), "http://192.0.2.1:8080".to_string())]),
    inject: BTreeMap::from([("model.needy".to_string(), ("authorization".to_string(), "Bearer test-secret".to_string()))]),
  }
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
fn a_host_matching_a_pattern_or_mapped_to_an_upstream_is_allowed() {
  let chain = Chain::new(&policy(), None);

  assert_eq!(chain.decide(&request("api.example.com")).outcome, NetOutcome::Allow);
  assert_eq!(chain.decide(&request("example.com")).outcome, NetOutcome::Allow);
  assert_eq!(chain.decide(&request("model.needy")).outcome, NetOutcome::Allow);
  assert_eq!(chain.decide(&request("example.org")).outcome, NetOutcome::Deny);
}

#[test]
fn only_a_request_to_its_host_gets_the_injected_header() {
  let chain = Chain::new(&policy(), None);

  assert_eq!(chain.decide(&request("model.needy")).headers, [("authorization".to_string(), "Bearer test-secret".to_string())]);
  assert!(chain.decide(&request("api.example.com")).headers.is_empty());
  // A tunnel is opaque.
  assert!(chain.decide(&tunnel("model.needy")).headers.is_empty());
}

#[test]
fn a_request_chaos_failed_gets_no_injected_header() {
  let chaos = serde_yaml::from_str("faults: [{on: net.connect, match: model.needy, errno: ECONNRESET}]").unwrap();
  let chain = Chain::new(&policy(), Some(Arc::new(Injector::new(&chaos))));

  let decision = chain.decide(&request("model.needy"));

  assert_eq!(decision.outcome, NetOutcome::Fault("ECONNRESET".into()));
  assert!(decision.headers.is_empty());
}
