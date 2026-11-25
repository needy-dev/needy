//! Credentials for a registry from the Docker credential store, and the
//! exchange of an identity token for an access token.
//!
//! A username and password (from `auths`, a `credHelpers` entry or the
//! `credsStore`) become `oci-client`'s Basic credentials, which it sends on
//! a `Basic` challenge (ECR) or to the token service of a `Bearer`
//! challenge (Harbor, Distribution, GHCR).
//!
//! An `identitytoken` (or a helper's `<token>` username) is an OAuth 2
//! refresh token: "IdentityToken is used to authenticate the user and get
//! an access token for the registry" (docker/cli `AuthConfig`). It is POSTed
//! to the `Bearer` challenge's realm with `grant_type=refresh_token`, and the
//! access token the service answers with is what `oci-client` sends, as a
//! Bearer token. The refresh token itself never goes to the registry.
// Source: https://github.com/docker/cli/blob/v28.3.3/cli/config/types/authconfig.go#L16-L21
// Source: https://github.com/distribution/distribution/blob/v3.0.0/docs/content/spec/auth/oauth.md
// (the `refresh_token` grant: `grant_type`, `refresh_token`, `service`,
// `scope`, `client_id`; the answer's `access_token`).
// Source: https://github.com/containers/image/blob/v5.36.0/docker/docker_client.go#L767-L812
// (`getBearerTokenOAuth2`, the same exchange) and #L860-L885 (the answer's
// `token` or `access_token`).
// Source: https://docs.rs/crate/docker_credential/1.4.0/source/src/helper.rs (lines 50-51)
// (a helper's `<token>` username is an identity token).
// NOTICE: `oci-client` 0.15 sends a `RegistryAuth::Bearer` as-is to the
// registry (`src/client.rs`, `_auth`), and has no refresh-token grant (its
// own TODO there); exchanging first and handing it the access token is the
// fix. Remove this exchange when `oci-client` does the grant itself.
// NOTICE: an exchanged access token is not refreshed: `oci-client` keeps a
// Bearer token for the client's life, so a connection is reused only until
// the token's `expires_in` (60 seconds when absent, token.md of the
// distribution spec above) and a new `Access::connect` exchanges again. One
// operation longer than the token's life (a very large blob) can still
// outlive it.

use std::path::Path;
use std::time::Duration;

use docker_credential::{CredentialRetrievalError, DockerCredential};
use oci_client::secrets::RegistryAuth;

use crate::{Error, Operation, Result};

/// The client identifier the token service is told, as the spec asks a
/// client to name itself.
const CLIENT_ID: &str = "needy";

/// How long before an exchanged token's expiry a connection stops being
/// reused.
const EXPIRY_MARGIN: Duration = Duration::from_secs(10);

/// How long the identity-token exchange may take, each request.
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(30);

/// The TLS settings the exchange shares with the registry client.
pub(crate) struct Tls<'a> {
  pub roots: &'a [Vec<u8>],
  pub insecure: bool,
  pub no_proxy: bool,
  pub user_agent: &'static str,
}

/// The credentials to use for `operation` on `repository` at `host`, and
/// how long they last when they say so (an exchanged access token).
pub(crate) async fn resolve(
  docker_config: Option<&Path>,
  host: &str,
  repository: &str,
  operation: Operation,
  tls: &Tls<'_>,
) -> Result<(RegistryAuth, Option<Duration>)> {
  match lookup(docker_config, host).await {
    None => Ok((RegistryAuth::Anonymous, None)),
    Some(DockerCredential::UsernamePassword(username, password)) => Ok((RegistryAuth::Basic(username, password), None)),
    Some(DockerCredential::IdentityToken(refresh_token)) => exchange(host, repository, operation, &refresh_token, tls).await,
  }
}

/// What the Docker credential store holds for `host`, or `None` when it
/// holds nothing or cannot be read (anonymous access, as Docker does).
async fn lookup(docker_config: Option<&Path>, host: &str) -> Option<DockerCredential> {
  let docker_config = docker_config.map(Path::to_path_buf);
  let host = host.to_string();
  // A credential helper is a subprocess, so the lookup runs off the runtime.
  let found = tokio::task::spawn_blocking(move || match &docker_config {
    Some(dir) => {
      let path = dir.join("config.json");
      match std::fs::File::open(&path) {
        Ok(file) => docker_credential::get_credential_from_reader(std::io::BufReader::new(file), &host),
        Err(_) => Err(CredentialRetrievalError::ConfigReadError),
      }
    }
    None => docker_credential::get_credential(&host),
  })
  .await;
  match found {
    Ok(Ok(credential)) => Some(credential),
    Ok(Err(
      CredentialRetrievalError::ConfigNotFound
      | CredentialRetrievalError::ConfigReadError
      | CredentialRetrievalError::NoCredentialConfigured,
    )) => None,
    Ok(Err(error)) => {
      tracing::warn!("cannot read the Docker credential store's credentials, connecting anonymously: {error}");
      None
    }
    Err(error) => {
      tracing::warn!("the Docker credential store lookup failed, connecting anonymously: {error}");
      None
    }
  }
}

/// Exchanges `refresh_token` at `host`'s token service for an access token
/// scoped to `operation` on `repository`.
async fn exchange(
  host: &str,
  repository: &str,
  operation: Operation,
  refresh_token: &str,
  tls: &Tls<'_>,
) -> Result<(RegistryAuth, Option<Duration>)> {
  let failed = |reason: String| Error::IdentityToken {
    host: host.to_string(),
    reason,
  };
  let mut builder = reqwest::Client::builder()
    .use_rustls_tls()
    .danger_accept_invalid_certs(tls.insecure)
    .user_agent(tls.user_agent)
    .timeout(EXCHANGE_TIMEOUT);
  for pem in tls.roots {
    let certificate = reqwest::Certificate::from_pem(pem).map_err(|e| failed(format!("a CA file does not parse: {e}")))?;
    builder = builder.add_root_certificate(certificate);
  }
  if tls.no_proxy {
    builder = builder.no_proxy();
  }
  let client = builder.build().map_err(|e| failed(e.to_string()))?;

  // The challenge names the token service. This probe carries no
  // credentials.
  let probe = client.get(format!("https://{host}/v2/")).send().await.map_err(|e| failed(format!("GET /v2/: {e}")))?;
  if probe.status().is_success() {
    // A registry that asks for no credentials gets none.
    return Ok((RegistryAuth::Anonymous, None));
  }
  let challenges = probe
    .headers()
    .get_all(reqwest::header::WWW_AUTHENTICATE)
    .iter()
    .filter_map(|value| value.to_str().ok())
    .map(str::to_string)
    .collect::<Vec<_>>();
  let (realm, service) = bearer_challenge(&challenges)
    .ok_or_else(|| failed(format!("it answers {} without a Bearer token service to exchange an identity token at", probe.status())))?;
  // NOTICE: the refresh token is a credential, so it never goes over plain
  // HTTP, whatever realm the registry names.
  if !realm.starts_with("https://") {
    return Err(failed(format!("its token service {realm} is not HTTPS")));
  }

  let scope = format!("repository:{repository}:{}", operation.actions());
  let mut form = vec![
    ("grant_type", "refresh_token"),
    ("refresh_token", refresh_token),
    ("client_id", CLIENT_ID),
    ("scope", scope.as_str()),
  ];
  if let Some(service) = service.as_deref() {
    form.push(("service", service));
  }
  let answer = client.post(&realm).form(&form).send().await.map_err(|e| failed(format!("POST {realm}: {e}")))?;
  let status = answer.status();
  // NOTICE: the OAuth spec lets a token service without the POST grant
  // answer 404, after which a client falls back to the GET flow; that flow
  // takes a username and password, which an identity token is not, so a
  // refusal of any kind is the error.
  if !status.is_success() {
    return Err(failed(format!("its token service {realm} answered {status}")));
  }
  #[derive(serde::Deserialize)]
  struct Answer {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
  }
  let answer: Answer = answer.json().await.map_err(|e| failed(format!("its token service {realm} answered: {e}")))?;
  // The token spec's default lifetime is 60 seconds; a margin keeps a
  // reused connection from sending a token about to expire.
  let lifetime = Duration::from_secs(answer.expires_in.unwrap_or(60)).saturating_sub(EXPIRY_MARGIN);
  match answer.access_token.or(answer.token).filter(|token| !token.is_empty()) {
    Some(access_token) => Ok((RegistryAuth::Bearer(access_token), Some(lifetime))),
    None => Err(failed(format!("its token service {realm} answered no access token"))),
  }
}

/// The realm and service of the first `Bearer` challenge in `headers`.
// Source: https://github.com/distribution/distribution/blob/v3.0.0/docs/content/spec/auth/token.md
// (`Www-Authenticate: Bearer realm="...",service="...",scope="..."`).
fn bearer_challenge(headers: &[String]) -> Option<(String, Option<String>)> {
  for header in headers {
    let Ok(challenges) = http_auth::parse_challenges(header) else {
      continue;
    };
    for challenge in challenges {
      if !challenge.scheme.eq_ignore_ascii_case("bearer") {
        continue;
      }
      let param =
        |name: &str| challenge.params.iter().find(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, value)| value.to_unescaped());
      if let Some(realm) = param("realm") {
        return Some((realm, param("service")));
      }
    }
  }
  None
}
