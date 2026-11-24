//! An in-memory OCI Distribution registry for tests of the layer registry,
//! served over plain HTTP on a loopback port.
//!
//! It implements what [`super::Registry`] and `oci-client` 0.15 use: the
//! `/v2/` probe, manifests by tag or digest (`GET`, `HEAD`, `PUT`), blobs
//! (`GET`, `HEAD`) and blob uploads (`POST`, chunked `PATCH`, closing `PUT`
//! with its `digest`, which it verifies), without authentication.
// NOTICE: hand-written because no maintained Rust crate serves an embeddable
// OCI Distribution registry for tests (a full registry such as
// `distribution/distribution` needs a container or a Go toolchain, which
// `cargo test` cannot assume). It is the subset of the specification the
// client above uses, not a conforming registry.
// Source: https://github.com/opencontainers/distribution-spec/blob/v1.1.0/spec.md#endpoints

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use http_body_util::{BodyExt as _, Full};
use hyper::body::Incoming;
use hyper::{Method, Request, Response, StatusCode};
use sha2::Digest as _;

use super::Digest;

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
  /// Requests served, by method and path, in order.
  requests: Vec<String>,
}

/// A running test registry. It stops when dropped.
pub struct TestRegistry {
  address: SocketAddr,
  state: Arc<Mutex<State>>,
  task: tokio::task::JoinHandle<()>,
}

impl TestRegistry {
  /// Starts a registry on a loopback port.
  pub async fn start() -> Self {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind a loopback port");
    let address = listener.local_addr().expect("a bound listener has an address");
    let state: Arc<Mutex<State>> = Arc::default();
    let shared = state.clone();
    let task = tokio::spawn(async move {
      while let Ok((stream, _)) = listener.accept().await {
        let state = shared.clone();
        tokio::spawn(async move {
          let service = hyper::service::service_fn(move |request| {
            let state = state.clone();
            async move { Ok::<_, Infallible>(serve(&state, request).await) }
          });
          // A client that hangs up mid-request is not a test failure.
          let _ = hyper::server::conn::http1::Builder::new().serve_connection(hyper_util::rt::TokioIo::new(stream), service).await;
        });
      }
    });
    Self {
      address,
      state,
      task,
    }
  }

  /// The repository `path` on this registry, as an operator names a
  /// plain-HTTP one: `http://127.0.0.1:PORT/PATH`.
  pub fn repository(&self, path: &str) -> String {
    format!("http://{}/{path}", self.address)
  }

  /// The registry's host and port.
  pub fn host(&self) -> String {
    self.address.to_string()
  }

  /// The requests served so far, as `METHOD /path`.
  pub fn requests(&self) -> Vec<String> {
    self.state.lock().unwrap_or_else(|p| p.into_inner()).requests.clone()
  }

  /// The tags of `repository`.
  pub fn tags(&self, repository: &str) -> Vec<String> {
    let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
    let prefix = format!("{repository}:");
    let mut tags: Vec<_> = state.tags.keys().filter_map(|key| key.strip_prefix(&prefix).map(str::to_string)).collect();
    tags.sort();
    tags
  }
}

impl Drop for TestRegistry {
  fn drop(&mut self) {
    self.task.abort();
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
  Digest::from_bytes(sha2::Sha256::digest(bytes).into()).to_string()
}

async fn serve(state: &Mutex<State>, request: Request<Incoming>) -> Reply {
  let method = request.method().clone();
  let path = request.uri().path().to_string();
  let query = request.uri().query().unwrap_or("").to_string();
  let content_type = request.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
  let body = match request.into_body().collect().await {
    Ok(body) => body.to_bytes(),
    Err(_) => return error(StatusCode::BAD_REQUEST, "BLOB_UPLOAD_INVALID"),
  };
  let mut state = state.lock().unwrap_or_else(|p| p.into_inner());
  state.requests.push(format!("{method} {path}"));

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
