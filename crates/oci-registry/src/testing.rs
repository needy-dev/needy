//! An in-memory OCI Distribution registry for tests, on a loopback port,
//! over plain HTTP or over TLS with a certificate from a self-signed CA,
//! with the authentication shapes real registries use ([`Auth`]).
//!
//! It implements what `oci-client` 0.15 and needy's clients use: the
//! `/v2/` probe, manifests by tag or digest (`GET`, `HEAD`, `PUT`), blobs
//! (`GET`, `HEAD`) and blob uploads (`POST`, chunked `PATCH`, closing `PUT`
//! with its `digest`, which it verifies), and a token service at `/token`.
// NOTICE: hand-written because no maintained Rust crate serves an embeddable
// OCI Distribution registry for tests (a full registry such as
// `distribution/distribution` needs a container or a Go toolchain, which
// `cargo test` cannot assume). It is the subset of the specification the
// clients use, not a conforming registry.
// Source: https://github.com/opencontainers/distribution-spec/blob/v1.1.0/spec.md#endpoints
// Source: https://github.com/distribution/distribution/blob/v3.0.0/docs/content/spec/auth/token.md
// (challenge and token answer) and `oauth.md` (the `refresh_token` grant).

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use http_body_util::{BodyExt as _, Full};
use hyper::body::Incoming;
use hyper::{Method, Request, Response, StatusCode};
use sha2::Digest as _;

/// The token the test token service issues for credentials it accepts.
pub const ISSUED_TOKEN: &str = "test-registry-access-token";

/// How the registry asks for credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Auth {
  /// None: every request is served.
  None,
  /// ECR's shape: requests without these Basic credentials are answered
  /// `401` with a `Basic` challenge.
  Basic { username: String, password: String },
  /// Harbor's (and Distribution's) shape: requests without the issued
  /// Bearer token are answered `401` with a `Bearer` challenge; the token
  /// service (`GET /token`) issues [`ISSUED_TOKEN`] for these Basic
  /// credentials only.
  Token { username: String, password: String },
  /// A token service with the OAuth 2 refresh-token grant: `POST /token`
  /// with `grant_type=refresh_token` and this refresh token answers
  /// `access_token`, [`ISSUED_TOKEN`]; `GET /token` refuses everything.
  RefreshToken { refresh_token: String },
}

/// One request the registry saw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
  pub method: String,
  pub path: String,
  /// The `Authorization` header, if any.
  pub authorization: Option<String>,
  /// The body of a token-service request (others are not kept).
  pub token_request: Option<String>,
}

#[derive(Default)]
struct State {
  /// Blobs by digest (`sha256:<hex>`), shared by every repository.
  blobs: HashMap<String, Bytes>,
  /// Manifests by `<repository>@<digest>`, with their media type.
  manifests: HashMap<String, (String, Bytes)>,
  /// Tags: `<repository>:<tag>` to a manifest digest.
  tags: HashMap<String, String>,
  /// Uploads in progress by ID.
  uploads: HashMap<String, Vec<u8>>,
  next_upload: u64,
  seen: Vec<Seen>,
}

/// A certificate authority and the registry's certificate it signed.
struct Tls {
  ca_pem: String,
  acceptor: tokio_rustls::TlsAcceptor,
}

/// A running test registry. It stops when dropped.
pub struct TestRegistry {
  address: SocketAddr,
  tls: bool,
  ca_pem: Option<String>,
  state: Arc<Mutex<State>>,
  task: tokio::task::JoinHandle<()>,
}

impl TestRegistry {
  /// A plain-HTTP registry without authentication.
  pub async fn start() -> Self {
    Self::start_with(false, Auth::None).await
  }

  /// A registry over TLS (a certificate for `127.0.0.1` and `localhost`
  /// from a fresh self-signed CA, [`TestRegistry::ca_pem`]) or plain HTTP,
  /// asking for credentials as `auth` says.
  pub async fn start_with(tls: bool, auth: Auth) -> Self {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind a loopback port");
    let address = listener.local_addr().expect("a bound listener has an address");
    let tls = tls.then(self_signed);
    let ca_pem = tls.as_ref().map(|tls| tls.ca_pem.clone());
    let scheme = if tls.is_some() { "https" } else { "http" };
    let realm = format!("{scheme}://{address}/token");
    let state: Arc<Mutex<State>> = Arc::default();
    let shared = state.clone();
    let task = tokio::spawn(async move {
      let auth = Arc::new(auth);
      let acceptor = tls.map(|tls| tls.acceptor);
      while let Ok((stream, _)) = listener.accept().await {
        let state = shared.clone();
        let auth = auth.clone();
        let realm = realm.clone();
        let acceptor = acceptor.clone();
        tokio::spawn(async move {
          let service = hyper::service::service_fn(move |request| {
            let state = state.clone();
            let auth = auth.clone();
            let realm = realm.clone();
            async move { Ok::<_, Infallible>(serve(&state, &auth, &realm, request).await) }
          });
          let http = hyper::server::conn::http1::Builder::new();
          // A client that hangs up or refuses the certificate is not a
          // test failure; the client's own result says what happened.
          match acceptor {
            Some(acceptor) => {
              if let Ok(stream) = acceptor.accept(stream).await {
                let _ = http.serve_connection(hyper_util::rt::TokioIo::new(stream), service).await;
              }
            }
            None => {
              let _ = http.serve_connection(hyper_util::rt::TokioIo::new(stream), service).await;
            }
          }
        });
      }
    });
    Self {
      address,
      tls: ca_pem.is_some(),
      ca_pem,
      state,
      task,
    }
  }

  /// The registry's host and port, as references name it.
  pub fn host(&self) -> String {
    self.address.to_string()
  }

  /// The repository `path` on this registry as an operator names it:
  /// `HOST:PORT/PATH` over TLS, `http://HOST:PORT/PATH` over plain HTTP.
  pub fn repository(&self, path: &str) -> String {
    if self.tls {
      format!("{}/{path}", self.address)
    } else {
      format!("http://{}/{path}", self.address)
    }
  }

  /// The PEM of the CA that signed the registry's certificate (TLS only).
  pub fn ca_pem(&self) -> Option<&str> {
    self.ca_pem.as_deref()
  }

  /// Writes the CA as `<root>/<host:port>/ca.crt` (`<hostport>` on
  /// Windows), as a `certs.d` root holds it, and returns `root`.
  pub fn write_certs_d(&self, root: &Path) -> PathBuf {
    let name = crate::certs::host_directories(&self.host()).pop().expect("a host has a directory name");
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).expect("create the certs.d directory");
    std::fs::write(dir.join("ca.crt"), self.ca_pem().expect("a TLS registry has a CA")).expect("write the CA file");
    root.to_path_buf()
  }

  /// The requests served so far, in order.
  pub fn seen(&self) -> Vec<Seen> {
    self.state.lock().unwrap_or_else(|p| p.into_inner()).seen.clone()
  }

  /// The requests served so far, as `METHOD /path`.
  pub fn requests(&self) -> Vec<String> {
    self.seen().into_iter().map(|seen| format!("{} {}", seen.method, seen.path)).collect()
  }

  /// The tags of `repository`, sorted.
  pub fn tags(&self, repository: &str) -> Vec<String> {
    let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
    let prefix = format!("{repository}:");
    let mut tags: Vec<_> = state.tags.keys().filter_map(|key| key.strip_prefix(&prefix).map(str::to_string)).collect();
    tags.sort();
    tags
  }

  /// The manifest `repository:tag` names, with its media type.
  pub fn manifest(&self, repository: &str, tag: &str) -> Option<(String, Vec<u8>)> {
    let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
    let digest = state.tags.get(&format!("{repository}:{tag}"))?;
    state.manifests.get(&format!("{repository}@{digest}")).map(|(media_type, bytes)| (media_type.clone(), bytes.to_vec()))
  }

  /// The blob `digest`.
  pub fn blob(&self, digest: &str) -> Option<Vec<u8>> {
    self.state.lock().unwrap_or_else(|p| p.into_inner()).blobs.get(digest).map(|bytes| bytes.to_vec())
  }

  /// Stores `bytes` as a blob, unchecked, and returns its digest.
  pub fn put_blob(&self, bytes: &[u8]) -> String {
    let digest = sha256(bytes);
    self.state.lock().unwrap_or_else(|p| p.into_inner()).blobs.insert(digest.clone(), Bytes::copy_from_slice(bytes));
    digest
  }

  /// Replaces the blob `digest` with `bytes`, which need not match it: a
  /// registry that serves tampered content.
  pub fn tamper_blob(&self, digest: &str, bytes: &[u8]) {
    self.state.lock().unwrap_or_else(|p| p.into_inner()).blobs.insert(digest.to_string(), Bytes::copy_from_slice(bytes));
  }

  /// Stores `bytes` as a manifest of `media_type` tagged `repository:tag`,
  /// unchecked, and returns its digest.
  pub fn put_manifest(&self, repository: &str, tag: &str, media_type: &str, bytes: &[u8]) -> String {
    let digest = sha256(bytes);
    let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
    state.manifests.insert(format!("{repository}@{digest}"), (media_type.to_string(), Bytes::copy_from_slice(bytes)));
    state.tags.insert(format!("{repository}:{tag}"), digest.clone());
    digest
  }
}

impl Drop for TestRegistry {
  fn drop(&mut self) {
    self.task.abort();
  }
}

/// A fresh CA, and a TLS acceptor with a certificate it signed for
/// `127.0.0.1` and `localhost`.
fn self_signed() -> Tls {
  use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair, KeyUsagePurpose};

  let ca_key = KeyPair::generate().expect("generate a CA key");
  let mut ca_params = CertificateParams::new(Vec::<String>::new()).expect("CA parameters");
  ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
  ca_params.distinguished_name.push(DnType::CommonName, "needy test registry CA");
  ca_params.key_usages = vec![
    KeyUsagePurpose::KeyCertSign,
    KeyUsagePurpose::CrlSign,
    KeyUsagePurpose::DigitalSignature,
  ];
  let ca = ca_params.self_signed(&ca_key).expect("self-sign the CA");
  let issuer = Issuer::new(ca_params, ca_key);

  let key = KeyPair::generate().expect("generate the registry's key");
  let mut params = CertificateParams::new(vec!["127.0.0.1".to_string(), "localhost".to_string()]).expect("registry parameters");
  params.distinguished_name.push(DnType::CommonName, "needy test registry");
  let certificate = params.signed_by(&key, &issuer).expect("sign the registry's certificate");

  let provider = Arc::new(rustls::crypto::ring::default_provider());
  let config = rustls::ServerConfig::builder_with_provider(provider)
    .with_safe_default_protocol_versions()
    .expect("ring supports the default protocol versions")
    .with_no_client_auth()
    .with_single_cert(vec![certificate.der().clone()], rustls::pki_types::PrivateKeyDer::Pkcs8(key.serialize_der().into()))
    .expect("a valid certificate and key");
  Tls {
    ca_pem: ca.pem(),
    acceptor: tokio_rustls::TlsAcceptor::from(Arc::new(config)),
  }
}

type Reply = Response<Full<Bytes>>;

fn reply(status: StatusCode, headers: &[(&str, String)], body: impl Into<Bytes>) -> Reply {
  let mut response = Response::builder().status(status);
  for (name, value) in headers {
    response = response.header(*name, value);
  }
  response.body(Full::new(body.into())).expect("valid response parts")
}

/// An OCI error envelope with one error.
fn error(status: StatusCode, code: &str) -> Reply {
  let body = serde_json::json!({ "errors": [{ "code": code, "message": code.to_lowercase() }] });
  reply(status, &[("Content-Type", "application/json".into())], body.to_string())
}

fn sha256(bytes: &[u8]) -> String {
  let digest: [u8; 32] = sha2::Sha256::digest(bytes).into();
  let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
  format!("sha256:{hex}")
}

/// RFC 7617 Basic credentials as a client sends them.
fn basic(username: &str, password: &str) -> String {
  use base64::Engine as _;
  format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}")))
}

fn form_value(body: &str, name: &str) -> Option<String> {
  body.split('&').find_map(|pair| {
    let (key, value) = pair.split_once('=')?;
    (key == name).then(|| value.replace("%3A", ":").replace("%2C", ",").replace("%2F", "/").replace('+', " "))
  })
}

async fn serve(state: &Mutex<State>, auth: &Auth, realm: &str, request: Request<Incoming>) -> Reply {
  let method = request.method().clone();
  let path = request.uri().path().to_string();
  let query = request.uri().query().unwrap_or("").to_string();
  let content_type = request.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
  let authorization = request.headers().get("authorization").and_then(|v| v.to_str().ok()).map(str::to_string);
  let body = match request.into_body().collect().await {
    Ok(body) => body.to_bytes(),
    Err(_) => return error(StatusCode::BAD_REQUEST, "BLOB_UPLOAD_INVALID"),
  };
  let token_request = (path == "/token").then(|| {
    if method == Method::POST {
      String::from_utf8_lossy(&body).to_string()
    } else {
      query.clone()
    }
  });
  let mut state = state.lock().unwrap_or_else(|p| p.into_inner());
  state.seen.push(Seen {
    method: method.to_string(),
    path: path.clone(),
    authorization: authorization.clone(),
    token_request: token_request.clone(),
  });

  if path == "/token" {
    return token_service(auth, &method, authorization.as_deref(), token_request.as_deref().unwrap_or(""));
  }
  let bearer = format!("Bearer {ISSUED_TOKEN}");
  let allowed = match auth {
    Auth::None => true,
    Auth::Basic { username, password } => authorization.as_deref() == Some(basic(username, password).as_str()),
    Auth::Token { .. } | Auth::RefreshToken { .. } => authorization.as_deref() == Some(bearer.as_str()),
  };
  if !allowed {
    let challenge = match auth {
      Auth::Basic { .. } => "Basic realm=\"test registry\"".to_string(),
      _ => format!("Bearer realm=\"{realm}\",service=\"test-registry\""),
    };
    return reply(StatusCode::UNAUTHORIZED, &[("WWW-Authenticate", challenge)], "");
  }

  if path == "/v2/" || path == "/v2" {
    return reply(StatusCode::OK, &[], "{}");
  }
  let Some(rest) = path.strip_prefix("/v2/") else {
    return error(StatusCode::NOT_FOUND, "UNSUPPORTED");
  };

  // `<name>/blobs/uploads/[<id>]`
  if let Some((name, id)) = rest.split_once("/blobs/uploads/").or_else(|| rest.strip_suffix("/blobs/uploads").map(|name| (name, ""))) {
    return match (method, id) {
      (Method::POST, "") => {
        state.next_upload += 1;
        let id = state.next_upload.to_string();
        state.uploads.insert(id.clone(), body.to_vec());
        let location = format!("/v2/{name}/blobs/uploads/{id}");
        reply(StatusCode::ACCEPTED, &[("Location", location), ("Range", "0-0".into())], "")
      }
      (Method::PATCH, id) => {
        let Some(upload) = state.uploads.get_mut(id) else {
          return error(StatusCode::NOT_FOUND, "BLOB_UPLOAD_UNKNOWN");
        };
        upload.extend_from_slice(&body);
        let end = upload.len().saturating_sub(1);
        let location = format!("/v2/{name}/blobs/uploads/{id}");
        reply(StatusCode::ACCEPTED, &[("Location", location), ("Range", format!("0-{end}"))], "")
      }
      (Method::PUT, id) => {
        let Some(mut upload) = state.uploads.remove(id) else {
          return error(StatusCode::NOT_FOUND, "BLOB_UPLOAD_UNKNOWN");
        };
        upload.extend_from_slice(&body);
        let Some(digest) = query.split('&').find_map(|pair| pair.strip_prefix("digest=")) else {
          return error(StatusCode::BAD_REQUEST, "DIGEST_INVALID");
        };
        // `oci-client` percent-encodes the `:` of the digest in the query.
        let digest = digest.replace("%3A", ":").replace("%3a", ":");
        if sha256(&upload) != digest {
          return error(StatusCode::BAD_REQUEST, "DIGEST_INVALID");
        }
        state.blobs.insert(digest.clone(), Bytes::from(upload));
        let location = format!("/v2/{name}/blobs/{digest}");
        reply(StatusCode::CREATED, &[("Location", location), ("Docker-Content-Digest", digest)], "")
      }
      _ => error(StatusCode::METHOD_NOT_ALLOWED, "UNSUPPORTED"),
    };
  }

  if let Some((_, digest)) = rest.split_once("/blobs/") {
    let Some(blob) = state.blobs.get(digest).cloned() else {
      return error(StatusCode::NOT_FOUND, "BLOB_UNKNOWN");
    };
    let headers = [
      ("Content-Type", "application/octet-stream".to_string()),
      ("Docker-Content-Digest", digest.to_string()),
    ];
    return match method {
      Method::GET => reply(StatusCode::OK, &headers, blob),
      Method::HEAD => reply(StatusCode::OK, &[("Content-Length", blob.len().to_string())], ""),
      _ => error(StatusCode::METHOD_NOT_ALLOWED, "UNSUPPORTED"),
    };
  }

  if let Some((name, reference)) = rest.split_once("/manifests/") {
    if method == Method::PUT {
      let digest = sha256(&body);
      state.manifests.insert(format!("{name}@{digest}"), (content_type, body));
      if !reference.starts_with("sha256:") {
        state.tags.insert(format!("{name}:{reference}"), digest.clone());
      }
      let location = format!("/v2/{name}/manifests/{digest}");
      return reply(StatusCode::CREATED, &[("Location", location), ("Docker-Content-Digest", digest)], "");
    }
    let digest = if reference.starts_with("sha256:") {
      reference.to_string()
    } else {
      match state.tags.get(&format!("{name}:{reference}")) {
        Some(digest) => digest.clone(),
        None => return error(StatusCode::NOT_FOUND, "MANIFEST_UNKNOWN"),
      }
    };
    let Some((media_type, manifest)) = state.manifests.get(&format!("{name}@{digest}")).cloned() else {
      return error(StatusCode::NOT_FOUND, "MANIFEST_UNKNOWN");
    };
    let headers = [
      ("Content-Type", media_type),
      ("Docker-Content-Digest", digest),
    ];
    return match method {
      Method::GET => reply(StatusCode::OK, &headers, manifest),
      Method::HEAD => reply(StatusCode::OK, &headers, ""),
      _ => error(StatusCode::METHOD_NOT_ALLOWED, "UNSUPPORTED"),
    };
  }

  error(StatusCode::NOT_FOUND, "UNSUPPORTED")
}

/// The token service at `/token`.
fn token_service(auth: &Auth, method: &Method, authorization: Option<&str>, request: &str) -> Reply {
  let issued = |field: &str| {
    let body = serde_json::json!({ field: ISSUED_TOKEN, "expires_in": 300 });
    reply(StatusCode::OK, &[("Content-Type", "application/json".into())], body.to_string())
  };
  match (auth, method) {
    (Auth::Token { username, password }, &Method::GET) if authorization == Some(basic(username, password).as_str()) => issued("token"),
    (Auth::RefreshToken { refresh_token }, &Method::POST)
      if form_value(request, "grant_type").as_deref() == Some("refresh_token")
        && form_value(request, "refresh_token").as_deref() == Some(refresh_token.as_str()) =>
    {
      issued("access_token")
    }
    _ => error(StatusCode::UNAUTHORIZED, "UNAUTHORIZED"),
  }
}
