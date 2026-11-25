//! Access to OCI Distribution registries the way container tools configure
//! it, for every OCI client in needy (Dev Container features, the layer
//! registry, and image imports):
//!
//! - **Credentials** from the Docker credential store: `$DOCKER_CONFIG/config.json`
//!   (or `~/.docker/config.json`), its `auths`, `credHelpers` and
//!   `credsStore`, through [`docker_credential`]. An `identitytoken` is a
//!   refresh token: it is exchanged at the registry's token service for an
//!   access token ([`credentials`]) and never sent to the registry itself.
//! - **Trust**: the system's trust store and the bundled WebPKI roots
//!   (`oci-client`'s `rustls-tls-native-roots` and `rustls-tls`), plus the
//!   CA files of the registry's host in containers-style `certs.d`
//!   directories ([`certs`]).
//! - **Insecure registries**: per host, TLS certificates are not verified
//!   ([`Access::with_insecure_registries`]); every connection warns.
//! - **Plain HTTP** is the caller's explicit choice for one endpoint and
//!   never carries credentials.
//!
//! Each [`Access::connect`] builds one `oci_client::Client` for one registry
//! host, since `oci-client`'s TLS settings are client-wide, and reads the
//! credential store again, so credentials a helper rotates are not kept.
//!
//! See `docs/ai/references/architecture/2026-11-25-layered-oci-base-spec.md`
//! (B4, "Settled in B-3").
// TODO(registry-mtls): client certificates (`*.cert`/`*.key` in a host's
// `certs.d` directory) are not presented: `oci-client` 0.15 (and 0.18) has
// no client-identity setting and takes no prepared `reqwest::Client`.
// Reopen when `oci-client` gains one, or a deployment needs mTLS badly
// enough to carry a fork.
// TODO(registry-mirrors): containerd's `hosts.toml` (mirrors, capabilities,
// `override_path`) and `registries.conf` are not read; needy talks to the
// registry a reference names. Reopen when a deployment needs a mirror.

pub mod certs;
pub mod credentials;
#[cfg(any(test, feature = "test-support"))]
pub mod testing;

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::{Duration, Instant};

use oci_client::client::{Certificate, CertificateEncoding, ClientConfig, ClientProtocol};
use oci_client::errors::OciDistributionError;
use oci_client::secrets::RegistryAuth;

/// What registry access could not set up.
#[derive(Debug, thiserror::Error)]
pub enum Error {
  /// Text that is not a registry host.
  #[error("{text:?} is not a registry host (`HOST[:PORT]`): {reason}")]
  Host { text: String, reason: String },
  /// A host's `certs.d` directory could not be read.
  #[error("cannot read the CA certificates of registry {host} in {}: {source}", path.display())]
  Certificates {
    host: String,
    path: PathBuf,
    #[source]
    source: std::io::Error,
  },
  /// `oci-client` refused the client configuration (an unreadable CA file).
  #[error("cannot set up a client for registry {host}: {source}")]
  Client {
    host: String,
    #[source]
    source: OciDistributionError,
  },
  /// The credential store holds an identity token for the registry, and it
  /// could not be exchanged for an access token.
  #[error("registry {host}: cannot exchange the identity token of the Docker credential store for an access token: {reason}")]
  IdentityToken { host: String, reason: String },
}

pub type Result<T> = std::result::Result<T, Error>;

/// How long a connection may be reused when its credentials carry no
/// expiry of their own (anonymous, or Basic from the credential store).
// NOTICE: bounded so that credentials a helper rotates (such as
// `docker-credential-ecr-login`'s 12-hour tokens) are read again while a
// caller keeps connections, and short enough that a rotated secret is
// picked up within minutes; `oci-client` itself refreshes the Bearer tokens
// it gets from a token service by their `expires_in`.
const REUSE_FOR: Duration = Duration::from_secs(5 * 60);

/// What a connection is for: the scope an exchanged access token asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
  Pull,
  Push,
}

impl Operation {
  /// The token scope's actions.
  // Source: https://github.com/distribution/distribution/blob/v3.0.0/docs/content/spec/auth/scope.md
  // ("repository:<name>:pull,push").
  fn actions(self) -> &'static str {
    match self {
      Self::Pull => "pull",
      Self::Push => "pull,push",
    }
  }
}

/// How this process reaches registries. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Access {
  insecure: BTreeSet<String>,
  cert_roots: Vec<PathBuf>,
  docker_config: Option<PathBuf>,
  no_proxy: bool,
  user_agent: &'static str,
}

impl Default for Access {
  fn default() -> Self {
    Self::standard()
  }
}

impl Access {
  /// The standard configuration: credentials from `DOCKER_CONFIG` (or
  /// `~/.docker`), CA files from [`certs::default_roots`], no insecure
  /// registries, proxies from the environment.
  pub fn standard() -> Self {
    Self {
      insecure: BTreeSet::new(),
      cert_roots: certs::default_roots(),
      docker_config: None,
      no_proxy: false,
      user_agent: concat!("needy/", env!("CARGO_PKG_VERSION")),
    }
  }

  /// Registries (`HOST[:PORT]`, as references name them) whose TLS
  /// certificates are not verified. Plain HTTP is a separate choice.
  pub fn with_insecure_registries<I, S>(mut self, hosts: I) -> Result<Self>
  where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
  {
    for host in hosts {
      self.insecure.insert(parse_host(host.as_ref())?);
    }
    Ok(self)
  }

  /// The `certs.d` directories to look a host's CA files up in, in order.
  pub fn with_cert_roots(mut self, roots: Vec<PathBuf>) -> Self {
    self.cert_roots = roots;
    self
  }

  /// Reads credentials from `dir/config.json` instead of `DOCKER_CONFIG`.
  pub fn with_docker_config(mut self, dir: impl Into<PathBuf>) -> Self {
    self.docker_config = Some(dir.into());
    self
  }

  /// Connects without HTTP proxies (loopback test registries).
  pub fn without_proxy(mut self) -> Self {
    self.no_proxy = true;
    self
  }

  /// The `User-Agent` every request sends.
  pub fn with_user_agent(mut self, user_agent: &'static str) -> Self {
    self.user_agent = user_agent;
    self
  }

  /// The registries whose certificates are not verified.
  pub fn insecure_registries(&self) -> impl Iterator<Item = &str> {
    self.insecure.iter().map(String::as_str)
  }

  /// Whether `host`'s certificates are not verified.
  pub fn is_insecure(&self, host: &str) -> bool {
    self.insecure.contains(host)
  }

  /// A client for `host` (`HOST[:PORT]`) and the credentials to use for
  /// `operation` on `repository`, already stored in the client. Over plain
  /// HTTP the credentials are always anonymous.
  pub async fn connect(&self, host: &str, plain_http: bool, repository: &str, operation: Operation) -> Result<Connection> {
    let insecure = !plain_http && self.is_insecure(host);
    if insecure {
      tracing::warn!("registry {host} is an insecure registry: its TLS certificate is not verified");
    }
    let roots = if plain_http {
      Vec::new()
    } else {
      certs::for_host(&self.cert_roots, host)?
    };
    let config = ClientConfig {
      protocol: if plain_http {
        ClientProtocol::Http
      } else {
        ClientProtocol::Https
      },
      accept_invalid_certificates: insecure,
      extra_root_certificates: roots
        .iter()
        .map(|pem| Certificate {
          encoding: CertificateEncoding::Pem,
          data: pem.clone(),
        })
        .collect(),
      user_agent: self.user_agent,
      no_proxy: self.no_proxy.then(|| "*".to_string()),
      ..Default::default()
    };
    // NOTICE: `Client::new` falls back to a default client (system roots
    // only, certificates verified) when the configuration fails; a CA file
    // that does not parse must be an error instead.
    let client = oci_client::Client::try_from(config).map_err(|source| Error::Client {
      host: host.to_string(),
      source,
    })?;
    // NOTICE: credentials are never sent over plain HTTP (spec B4).
    let (auth, lifetime) = if plain_http {
      (RegistryAuth::Anonymous, REUSE_FOR)
    } else {
      let tls = credentials::Tls {
        roots: &roots,
        insecure,
        no_proxy: self.no_proxy,
        user_agent: self.user_agent,
      };
      let (auth, lifetime) = credentials::resolve(self.docker_config.as_deref(), host, repository, operation, &tls).await?;
      (auth, lifetime.map_or(REUSE_FOR, |lifetime| lifetime.min(REUSE_FOR)))
    };
    let reference = oci_client::Reference::with_tag(host.to_string(), repository.to_string(), "latest".to_string());
    client.store_auth_if_needed(reference.resolve_registry(), &auth).await;
    Ok(Connection {
      client,
      auth,
      insecure,
      reuse_until: Instant::now() + lifetime,
    })
  }
}

/// A client for one registry host, with the credentials for one operation.
pub struct Connection {
  pub client: oci_client::Client,
  /// The credentials, as the client already holds them; pass them to the
  /// calls that take them (`pull_manifest_raw` and similar).
  pub auth: RegistryAuth,
  /// Whether the host's certificate is not verified.
  pub insecure: bool,
  /// Until when the connection may be reused for more operations of the
  /// same kind on the same repository: an exchanged access token's expiry,
  /// or a few minutes.
  pub reuse_until: Instant,
}

impl Connection {
  /// Whether the connection may still be reused.
  pub fn is_fresh(&self) -> bool {
    Instant::now() < self.reuse_until
  }
}

/// Checks that `text` is a registry host as a reference names it:
/// `HOST[:PORT]`, no scheme, no path.
pub fn parse_host(text: &str) -> Result<String> {
  let bad = |reason: &str| Error::Host {
    text: text.to_string(),
    reason: reason.to_string(),
  };
  if text.contains("://") {
    return Err(bad("name the host without a scheme"));
  }
  if text.is_empty() || text.contains('/') || text.chars().any(char::is_whitespace) {
    return Err(bad("a host has no path and no spaces"));
  }
  // The OCI reference grammar (`oci-spec`'s parser) checks the host; a
  // probe repository and tag prove that the text is only a host.
  let reference = oci_client::Reference::from_str(&format!("{text}/probe:probe")).map_err(|e| bad(&e.to_string()))?;
  if reference.registry() != text {
    return Err(bad("it is not a host a reference can name"));
  }
  Ok(text.to_string())
}

#[cfg(test)]
#[path = "lib_test.rs"]
mod tests;
