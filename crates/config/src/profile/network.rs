//! Network policy: the actor's network, which reaches the outside only
//! through the actor gateway, and the executor network.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::value::Glob;

/// The domain under which a service's in-actor host name lives:
/// `NAME.needy`. The gateway answers these names itself, so they need no
/// DNS in the actor; a public host under `.needy` cannot be reached.
pub const SERVICE_DOMAIN: &str = "needy";

/// The actor's network at its gateway.
// NOTICE: header injection (`inject:`) was removed on 2026-11-12 (owner
// decision): HTTPS is tunnelled as is, so a header could reach only
// plain-HTTP hosts, where the README's examples did not use it.
// TODO(mitm): header injection on HTTPS needs a separate MITM
// infrastructure (its own CA, opt-in, per host); deferred until that exists
// and the owner approves it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ActorNetwork {
  /// Hosts that may be reached, matched by [`Glob::matches_host`]: HTTPS
  /// is tunnelled as is, plain HTTP is forwarded.
  pub allow: Vec<Glob>,
  /// Named plain-HTTP services: the actor asks for `http://NAME.needy/...`
  /// and the gateway forwards the request to the service's `url`.
  pub services: BTreeMap<String, Service>,
}

impl ActorNetwork {
  pub fn allows(&self, host: &str) -> bool {
    self.allow.iter().any(|pattern| pattern.matches_host(host))
  }
}

/// A service the gateway forwards to.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Service {
  /// The base URL requests go to, `http://` or `https://`, resolved where
  /// the gateway runs: on the control host, next to the daemon.
  pub url: String,
}

/// The host name a service named `name` has in the actor.
pub fn service_host(name: &str) -> String {
  format!("{name}.{SERVICE_DOMAIN}")
}

/// The executor network.
// TODO(executor-network): executors have no gateway yet, so nothing
// enforces this; it waits for the owner to name the executor-network slice.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NetworkPolicy {
  /// Hosts that may be reached, matched by [`Glob::matches_host`].
  pub allow: Vec<Glob>,
  pub record: bool,
}

impl NetworkPolicy {
  pub fn allows(&self, host: &str) -> bool {
    self.allow.iter().any(|pattern| pattern.matches_host(host))
  }
}
