use std::io::Read as _;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use super::*;

fn make_feature_tar(metadata_json: &str, install_sh: &str) -> Vec<u8> {
  let mut tar = tar::Builder::new(Vec::new());
  for (name, content) in [
    ("devcontainer-feature.json", metadata_json.as_bytes()),
    ("install.sh", install_sh.as_bytes()),
  ] {
    let mut header = tar::Header::new_gnu();
    header.set_size(content.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append_data(&mut header, name, content).unwrap();
  }
  tar.into_inner().unwrap()
}

fn make_oci_manifest(layer_digest: &Digest, layer_size: usize) -> Vec<u8> {
  serde_json::to_vec(&serde_json::json!({
    "schemaVersion": 2,
    "mediaType": "application/vnd.oci.image.manifest.v1+json",
    "config": {
      "mediaType": "application/vnd.devcontainers",
      "digest": Digest::of(b"{}").to_string(),
      "size": 2
    },
    "layers": [
      {
        "mediaType": "application/vnd.devcontainers.layer.v1+tar",
        "digest": layer_digest.to_string(),
        "size": layer_size
      }
    ]
  }))
  .unwrap()
}

#[derive(Clone)]
struct MockRegistryState {
  repo: String,
  tag: String,
  manifest_bytes: Vec<u8>,
  manifest_digest: Digest,
  blob_digest: Digest,
  blob_bytes: Vec<u8>,
  required_token: String,
  requests: Arc<AtomicUsize>,
}

/// Spawns a local HTTP/1.1 mock OCI registry that enforces Bearer token auth
/// (`401 WWW-Authenticate: Bearer realm="...",service="...",scope="..."`),
/// serves `/token`, `/v2/<repo>/manifests/<tag|digest>`, and
/// `/v2/<repo>/blobs/<digest>`, and counts every incoming HTTP request.
async fn spawn_mock_registry(state: MockRegistryState) -> (String, Arc<AtomicUsize>) {
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let addr = listener.local_addr().unwrap();
  let base_url = format!("http://{addr}");
  let requests = state.requests.clone();
  let server_url = base_url.clone();

  tokio::spawn(async move {
    while let Ok((mut stream, _)) = listener.accept().await {
      let state = state.clone();
      let server_url = server_url.clone();
      tokio::spawn(async move {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 1024];
        while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
          match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
          }
        }
        state.requests.fetch_add(1, Ordering::SeqCst);

        let raw = String::from_utf8_lossy(&buf);
        let mut lines = raw.lines();
        let req_line = lines.next().unwrap_or("");
        let target = req_line.split_whitespace().nth(1).unwrap_or("/");
        let auth = lines
          .find(|line| line.to_ascii_lowercase().starts_with("authorization:"))
          .and_then(|line| line.split_once(':'))
          .map(|(_, v)| v.trim().to_string());

        let expected_auth = format!("Bearer {}", state.required_token);
        let manifest_by_tag = format!("/v2/{}/manifests/{}", state.repo, state.tag);
        let manifest_by_digest = format!("/v2/{}/manifests/{}", state.repo, state.manifest_digest);
        let blob_path = format!("/v2/{}/blobs/{}", state.repo, state.blob_digest);

        let (status, extra_headers, body) = if target.starts_with("/token") {
          let body = serde_json::to_vec(&serde_json::json!({ "token": state.required_token })).unwrap();
          ("200 OK", "Content-Type: application/json\r\n".to_string(), body)
        } else if auth.as_deref() != Some(expected_auth.as_str()) {
          let challenge = format!(
            "WWW-Authenticate: Bearer realm=\"{server_url}/token\",service=\"registry.example.com\",scope=\"repository:{}:pull\"\r\n",
            state.repo
          );
          ("401 Unauthorized", challenge, b"unauthorized".to_vec())
        } else if target == manifest_by_tag || target == manifest_by_digest {
          let headers =
            format!("Content-Type: application/vnd.oci.image.manifest.v1+json\r\nDocker-Content-Digest: {}\r\n", state.manifest_digest);
          ("200 OK", headers, state.manifest_bytes.clone())
        } else if target == blob_path {
          ("200 OK", "Content-Type: application/vnd.devcontainers.layer.v1+tar\r\n".to_string(), state.blob_bytes.clone())
        } else {
          ("404 Not Found", String::new(), b"not found".to_vec())
        };

        let head = format!("HTTP/1.1 {status}\r\n{extra_headers}Content-Length: {}\r\nConnection: close\r\n\r\n", body.len());
        if stream.write_all(head.as_bytes()).await.is_ok() {
          let _ = stream.write_all(&body).await;
        }
      });
    }
  });

  (base_url, requests)
}

#[test]
fn reference_parsing_handles_tags_digests_defaults_ports_and_canonical_identity() {
  let with_tag = Reference::parse("ghcr.io/devcontainers/features/node:1").unwrap();
  assert_eq!(with_tag.registry, "ghcr.io");
  assert_eq!(with_tag.repository, "devcontainers/features/node");
  assert_eq!(with_tag.tag(), Some("1"));
  assert_eq!(with_tag.digest(), None);
  assert_eq!(with_tag.canonical(), "ghcr.io/devcontainers/features/node");

  let semver = Reference::parse("ghcr.io/devcontainers/features/git:1.3.0").unwrap();
  assert_eq!(semver.tag(), Some("1.3.0"));
  assert_eq!(semver.canonical(), "ghcr.io/devcontainers/features/git");

  let digest = Digest::of(b"feature-manifest");
  let pinned = Reference::parse(&format!("ghcr.io/devcontainers/features/node@{digest}")).unwrap();
  assert_eq!(pinned.registry, "ghcr.io");
  assert_eq!(pinned.repository, "devcontainers/features/node");
  assert_eq!(pinned.tag(), None);
  assert_eq!(pinned.digest(), Some(digest));
  assert_eq!(pinned.canonical(), "ghcr.io/devcontainers/features/node");

  let tag_and_digest = Reference::parse(&format!("ghcr.io/devcontainers/features/node:1@{digest}")).unwrap();
  assert_eq!(tag_and_digest.digest(), Some(digest));
  assert_eq!(tag_and_digest.canonical(), "ghcr.io/devcontainers/features/node");

  let default_tag = Reference::parse("ghcr.io/devcontainers/features/node").unwrap();
  assert_eq!(default_tag.tag(), Some("latest"));
  assert_eq!(default_tag.canonical(), "ghcr.io/devcontainers/features/node");

  let with_port = Reference::parse("127.0.0.1:5000/devcontainers/features/node").unwrap();
  assert_eq!(with_port.registry, "127.0.0.1:5000");
  assert_eq!(with_port.repository, "devcontainers/features/node");
  assert_eq!(with_port.tag(), Some("latest"));
  assert_eq!(with_port.canonical(), "127.0.0.1:5000/devcontainers/features/node");

  assert_eq!(canonical_ref("ghcr.io/devcontainers/features/node:2").unwrap(), "ghcr.io/devcontainers/features/node");

  // Invalid references are rejected cleanly.
  assert!(Reference::parse("").is_err());
  assert!(Reference::parse("ghcr.io").is_err());
  assert!(Reference::parse("ghcr.io/devcontainers/features/node:").is_err());
  assert!(Reference::parse("ghcr.io/devcontainers/features/node@sha256:bad").is_err());
  assert!(Reference::parse("https://ghcr.io/devcontainers/features/node:1").is_err());
  assert!(Reference::parse("ghcr.io/../escape:1").is_err());
}

#[tokio::test]
async fn fetch_handles_bearer_auth_verifies_sha256_and_caches_atomically() {
  let metadata = r#"{"id":"node","version":"1.5.0","name":"Node.js"}"#;
  let blob_bytes = make_feature_tar(metadata, "#!/bin/sh\necho installing node\n");
  let blob_digest = Digest::of(&blob_bytes);
  let manifest_bytes = make_oci_manifest(&blob_digest, blob_bytes.len());
  let manifest_digest = Digest::of(&manifest_bytes);

  let (base_url, requests) = spawn_mock_registry(MockRegistryState {
    repo: "devcontainers/features/node".into(),
    tag: "1".into(),
    manifest_bytes,
    manifest_digest,
    blob_digest,
    blob_bytes: blob_bytes.clone(),
    required_token: "test-bearer-token-123".into(),
    requests: Arc::new(AtomicUsize::new(0)),
  })
  .await;

  let cache = tempfile::tempdir().unwrap();
  let store = FeatureStore::new(cache.path()).unwrap().without_proxy().unwrap().with_base_url(&base_url);

  let reference = Reference::parse("ghcr.io/devcontainers/features/node:1").unwrap();
  let fetched = store.fetch(&reference).await.unwrap();

  assert_eq!(fetched.digest, manifest_digest);
  assert_eq!(fetched.metadata_json, metadata);
  assert_eq!(fetched.archive_path, cache.path().join("features").join(format!("sha256-{}", manifest_digest.hex())).join("feature.tar"));
  assert_eq!(std::fs::read(&fetched.archive_path).unwrap(), blob_bytes);
  assert_eq!(std::fs::read_to_string(fetched.archive_path.parent().unwrap().join("devcontainer-feature.json")).unwrap(), metadata);
  // 401 manifest + token exchange + authorized manifest + authorized blob = 4 requests.
  assert_eq!(requests.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn cache_hit_by_pinned_digest_makes_zero_http_requests() {
  let metadata = r#"{"id":"git","version":"1.3.0"}"#;
  let blob_bytes = make_feature_tar(metadata, "#!/bin/sh\nexit 0\n");
  let blob_digest = Digest::of(&blob_bytes);
  let manifest_bytes = make_oci_manifest(&blob_digest, blob_bytes.len());
  let manifest_digest = Digest::of(&manifest_bytes);

  let (base_url, requests) = spawn_mock_registry(MockRegistryState {
    repo: "devcontainers/features/git".into(),
    tag: "1.3.0".into(),
    manifest_bytes,
    manifest_digest,
    blob_digest,
    blob_bytes,
    required_token: "tok".into(),
    requests: Arc::new(AtomicUsize::new(0)),
  })
  .await;

  let cache = tempfile::tempdir().unwrap();
  let store = FeatureStore::new(cache.path()).unwrap().without_proxy().unwrap().with_base_url(&base_url);

  // First fetch populates the cache.
  let tag_ref = Reference::parse("ghcr.io/devcontainers/features/git:1.3.0").unwrap();
  let first = store.fetch(&tag_ref).await.unwrap();
  assert_eq!(first.digest, manifest_digest);
  requests.store(0, Ordering::SeqCst);

  // Direct cache lookup hits with zero network.
  let cached = store.get_cached(&manifest_digest).unwrap().expect("expected cache hit");
  assert_eq!(cached, first);
  assert_eq!(requests.load(Ordering::SeqCst), 0);

  // Fetching by pinned `@sha256:...` reference hits cache with zero HTTP requests.
  let pinned_ref = Reference::parse(&format!("ghcr.io/devcontainers/features/git@{manifest_digest}")).unwrap();
  let second = store.fetch(&pinned_ref).await.unwrap();
  assert_eq!(second, first);
  assert_eq!(requests.load(Ordering::SeqCst), 0);

  // `fetch_pinned` with the tag ref + lockfile digest also makes zero HTTP requests.
  let third = store.fetch_pinned(&tag_ref, &manifest_digest).await.unwrap();
  assert_eq!(third, first);
  assert_eq!(requests.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn corrupt_blob_sha256_fails_cleanly_and_leaves_no_partial_cache() {
  let metadata = r#"{"id":"node","version":"1.5.0"}"#;
  let good_blob = make_feature_tar(metadata, "#!/bin/sh\nexit 0\n");
  let expected_blob_digest = Digest::of(&good_blob);
  let manifest_bytes = make_oci_manifest(&expected_blob_digest, good_blob.len());
  let manifest_digest = Digest::of(&manifest_bytes);

  // Serve tampered bytes under the expected blob URL.
  let mut corrupt_blob = good_blob;
  corrupt_blob[0] ^= 0xff;

  let (base_url, _requests) = spawn_mock_registry(MockRegistryState {
    repo: "devcontainers/features/node".into(),
    tag: "1".into(),
    manifest_bytes,
    manifest_digest,
    blob_digest: expected_blob_digest,
    blob_bytes: corrupt_blob,
    required_token: "tok".into(),
    requests: Arc::new(AtomicUsize::new(0)),
  })
  .await;

  let cache = tempfile::tempdir().unwrap();
  let store = FeatureStore::new(cache.path()).unwrap().without_proxy().unwrap().with_base_url(&base_url);
  let reference = Reference::parse("ghcr.io/devcontainers/features/node:1").unwrap();

  let err = store.fetch(&reference).await.unwrap_err();
  assert!(
    matches!(
      err,
      Error::DigestMismatch {
        what: "feature blob",
        ..
      }
    ),
    "unexpected error: {err:?}"
  );

  // Cache must not contain any entry or leftover temp directory.
  assert!(store.get_cached(&manifest_digest).unwrap().is_none());
  let features_dir = cache.path().join("features");
  if features_dir.exists() {
    let entries: Vec<_> = std::fs::read_dir(&features_dir).unwrap().collect();
    assert!(entries.is_empty(), "expected no partial cache files, found {entries:?}");
  }
}

#[test]
fn builtin_sshfs_has_valid_metadata_passes_check_allowed_and_packs_install_sh() {
  let feature = builtin_sshfs();
  assert_eq!(feature.reference, "builtin:sshfs");
  let metadata = needy_core::image::FeatureMetadata::parse(feature.metadata_json).unwrap();
  assert_eq!(metadata.id, "sshfs");
  metadata.check_allowed(feature.reference).unwrap();

  let archive = feature.archive().unwrap();
  let mut tar = tar::Archive::new(std::io::Cursor::new(archive));
  let mut saw_meta = false;
  let mut install_sh = String::new();
  for entry in tar.entries().unwrap() {
    let mut entry = entry.unwrap();
    let path = entry.path().unwrap().to_path_buf();
    if path == Path::new("devcontainer-feature.json") {
      saw_meta = true;
    } else if path == Path::new("install.sh") {
      entry.read_to_string(&mut install_sh).unwrap();
    }
  }
  assert!(saw_meta, "archive must contain devcontainer-feature.json");
  assert!(install_sh.contains("command -v sshfs"), "install.sh must check for existing sshfs: {install_sh}");
  assert!(install_sh.contains("apt-get"), "install.sh must support apt-get: {install_sh}");
  assert!(install_sh.contains("apk"), "install.sh must support apk: {install_sh}");
}

#[test]
fn all_embedded_builtin_features_are_valid_and_pack_executable_install_sh() {
  let expected_refs = [
    "builtin:sshfs",
    "builtin:codex",
    "builtin:pi",
    "builtin:opencode",
    "builtin:herdr",
  ];
  let all = all_builtins();
  let actual_refs: Vec<&str> = all.iter().map(|f| f.reference).collect();
  assert_eq!(actual_refs, expected_refs);

  for reference in expected_refs {
    let feature = builtin(reference).unwrap_or_else(|| panic!("missing builtin feature {reference}"));
    assert_eq!(feature.reference, reference);
    let metadata =
      needy_core::image::FeatureMetadata::parse(feature.metadata_json).unwrap_or_else(|e| panic!("invalid metadata for {reference}: {e}"));
    metadata.check_allowed(reference).unwrap_or_else(|e| panic!("check_allowed failed for {reference}: {e}"));

    let archive = feature.archive().unwrap();
    let mut tar = tar::Archive::new(std::io::Cursor::new(archive));
    let mut saw_meta = false;
    let mut saw_install_exec = false;
    for entry in tar.entries().unwrap() {
      let entry = entry.unwrap();
      let path = entry.path().unwrap().to_path_buf();
      if path == Path::new("devcontainer-feature.json") {
        saw_meta = true;
      } else if path == Path::new("install.sh") {
        let mode = entry.header().mode().unwrap();
        assert_eq!(mode & 0o111, 0o111, "install.sh in {reference} must be executable, got {mode:#o}");
        saw_install_exec = true;
      }
    }
    assert!(saw_meta, "{reference} archive must contain devcontainer-feature.json");
    assert!(saw_install_exec, "{reference} archive must contain executable install.sh");
  }

  // Agent features declare sync mappings for their state directories.
  let codex_meta = needy_core::image::FeatureMetadata::parse(builtin("builtin:codex").unwrap().metadata_json).unwrap();
  let codex_files = &codex_meta.customizations.needy.unwrap().files;
  assert_eq!(codex_files["codex"].path, "~/.codex");

  let pi_meta = needy_core::image::FeatureMetadata::parse(builtin("builtin:pi").unwrap().metadata_json).unwrap();
  let pi_files = &pi_meta.customizations.needy.unwrap().files;
  assert_eq!(pi_files["pi"].path, "~/.pi/agent");

  let opencode_builtin = builtin("builtin:opencode").unwrap();
  let opencode_meta = needy_core::image::FeatureMetadata::parse(opencode_builtin.metadata_json).unwrap();
  let opencode_files = &opencode_meta.customizations.needy.unwrap().files;
  assert_eq!(opencode_files["opencode"].path, "~/.local/share/opencode");
  assert_eq!(
    opencode_files["opencode"].on_conflict,
    vec![("opencode.db*".to_string(), needy_config::profile::OnConflict::Driver("needy-merge-sqlite".into()))]
  );
  let opencode_archive = opencode_builtin.archive().unwrap();
  let mut opencode_tar = tar::Archive::new(opencode_archive.as_slice());
  let mut saw_driver_exec = false;
  for entry in opencode_tar.entries().unwrap() {
    let entry = entry.unwrap();
    if entry.path().unwrap() == Path::new("needy-merge-sqlite") {
      let mode = entry.header().mode().unwrap();
      assert_eq!(mode & 0o111, 0o111, "needy-merge-sqlite must be executable, got {mode:#o}");
      saw_driver_exec = true;
    }
  }
  assert!(saw_driver_exec, "builtin:opencode archive must bundle executable needy-merge-sqlite");

  assert!(builtin("builtin:nonexistent").is_none());
}

#[test]
fn feature_store_fetch_builtin_caches_without_network() {
  let cache = tempfile::tempdir().unwrap();
  let store = FeatureStore::new(cache.path()).unwrap().without_proxy().unwrap();
  let fetched = store.fetch_builtin("builtin:codex").unwrap();
  assert_eq!(fetched.digest, builtin("builtin:codex").unwrap().digest());
  assert!(fetched.archive_path.is_file());
  let meta = needy_core::image::FeatureMetadata::parse(&fetched.metadata_json).unwrap();
  assert_eq!(meta.id, "codex");

  // Unknown builtin reference fails with InvalidReference.
  assert!(matches!(store.fetch_builtin("builtin:unknown"), Err(Error::InvalidReference(_, _))));
}

#[tokio::test]
async fn concurrent_fetches_share_one_in_flight_download() {
  let metadata = r#"{"id":"node","version":"1.5.0"}"#;
  let blob_bytes = make_feature_tar(metadata, "#!/bin/sh\nexit 0\n");
  let blob_digest = Digest::of(&blob_bytes);
  let manifest_bytes = make_oci_manifest(&blob_digest, blob_bytes.len());
  let manifest_digest = Digest::of(&manifest_bytes);

  let (base_url, requests) = spawn_mock_registry(MockRegistryState {
    repo: "devcontainers/features/node".into(),
    tag: "1".into(),
    manifest_bytes,
    manifest_digest,
    blob_digest,
    blob_bytes,
    required_token: "shared-tok".into(),
    requests: Arc::new(AtomicUsize::new(0)),
  })
  .await;

  let cache = tempfile::tempdir().unwrap();
  let store = FeatureStore::new(cache.path()).unwrap().without_proxy().unwrap().with_base_url(&base_url);
  let reference = Reference::parse("ghcr.io/devcontainers/features/node:1").unwrap();

  let mut handles = Vec::new();
  for _ in 0..4 {
    let store = store.clone();
    let reference = reference.clone();
    handles.push(tokio::spawn(async move { store.fetch(&reference).await }));
  }

  let mut results = Vec::new();
  for handle in handles {
    results.push(handle.await.unwrap().unwrap());
  }
  for fetched in &results {
    assert_eq!(fetched.digest, manifest_digest);
    assert_eq!(fetched.metadata_json, metadata);
  }
  // 401 manifest + token exchange + authorized manifest + authorized blob = 4 requests total.
  assert_eq!(requests.load(Ordering::SeqCst), 4, "concurrent fetches must share one in-flight download");
}

#[tokio::test]
async fn bearer_token_cache_reuses_token_across_fetches() {
  let meta1 = r#"{"id":"node","version":"1.0.0"}"#;
  let blob1 = make_feature_tar(meta1, "#!/bin/sh\necho 1\n");
  let blob1_digest = Digest::of(&blob1);
  let manifest1 = make_oci_manifest(&blob1_digest, blob1.len());
  let manifest1_digest = Digest::of(&manifest1);

  let meta2 = r#"{"id":"node","version":"2.0.0"}"#;
  let blob2 = make_feature_tar(meta2, "#!/bin/sh\necho 2\n");
  let blob2_digest = Digest::of(&blob2);
  let manifest2 = make_oci_manifest(&blob2_digest, blob2.len());
  let manifest2_digest = Digest::of(&manifest2);

  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let addr = listener.local_addr().unwrap();
  let base_url = format!("http://{addr}");
  let token_requests = Arc::new(AtomicUsize::new(0));
  let total_requests = Arc::new(AtomicUsize::new(0));
  let token_req_bg = token_requests.clone();
  let total_req_bg = total_requests.clone();
  let server_url = base_url.clone();

  tokio::spawn(async move {
    while let Ok((mut stream, _)) = listener.accept().await {
      let token_requests = token_req_bg.clone();
      let total_requests = total_req_bg.clone();
      let server_url = server_url.clone();
      let m1 = manifest1.clone();
      let b1 = blob1.clone();
      let m2 = manifest2.clone();
      let b2 = blob2.clone();
      tokio::spawn(async move {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 1024];
        while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
          match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
          }
        }
        total_requests.fetch_add(1, Ordering::SeqCst);
        let raw = String::from_utf8_lossy(&buf);
        let mut lines = raw.lines();
        let target = lines.next().unwrap_or("").split_whitespace().nth(1).unwrap_or("/");
        let auth = lines
          .find(|line| line.to_ascii_lowercase().starts_with("authorization:"))
          .and_then(|line| line.split_once(':'))
          .map(|(_, v)| v.trim().to_string());

        let (status, extra_headers, body) = if target.starts_with("/token") {
          token_requests.fetch_add(1, Ordering::SeqCst);
          let body = serde_json::to_vec(&serde_json::json!({ "token": "cached-tok", "expires_in": 300 })).unwrap();
          ("200 OK", "Content-Type: application/json\r\n".to_string(), body)
        } else if auth.as_deref() != Some("Bearer cached-tok") {
          let challenge = format!(
            "WWW-Authenticate: Bearer realm=\"{server_url}/token\",service=\"registry.example.com\",scope=\"repository:devcontainers/features/node:pull\"\r\n"
          );
          ("401 Unauthorized", challenge, b"unauthorized".to_vec())
        } else if target == "/v2/devcontainers/features/node/manifests/1" {
          ("200 OK", "Content-Type: application/vnd.oci.image.manifest.v1+json\r\n".to_string(), m1)
        } else if target == format!("/v2/devcontainers/features/node/blobs/{blob1_digest}") {
          ("200 OK", "Content-Type: application/vnd.devcontainers.layer.v1+tar\r\n".to_string(), b1)
        } else if target == "/v2/devcontainers/features/node/manifests/2" {
          ("200 OK", "Content-Type: application/vnd.oci.image.manifest.v1+json\r\n".to_string(), m2)
        } else if target == format!("/v2/devcontainers/features/node/blobs/{blob2_digest}") {
          ("200 OK", "Content-Type: application/vnd.devcontainers.layer.v1+tar\r\n".to_string(), b2)
        } else {
          ("404 Not Found", String::new(), b"not found".to_vec())
        };

        let head = format!("HTTP/1.1 {status}\r\n{extra_headers}Content-Length: {}\r\nConnection: close\r\n\r\n", body.len());
        if stream.write_all(head.as_bytes()).await.is_ok() {
          let _ = stream.write_all(&body).await;
        }
      });
    }
  });

  let cache = tempfile::tempdir().unwrap();
  let store = FeatureStore::new(cache.path()).unwrap().without_proxy().unwrap().with_base_url(&base_url);

  let first = store.fetch(&Reference::parse("ghcr.io/devcontainers/features/node:1").unwrap()).await.unwrap();
  assert_eq!(first.digest, manifest1_digest);
  assert_eq!(token_requests.load(Ordering::SeqCst), 1);
  assert_eq!(total_requests.load(Ordering::SeqCst), 4);

  // Second fetch on the same repository reuses the cached Bearer token:
  // zero `/token` requests and zero 401 round-trips (only manifest + blob).
  total_requests.store(0, Ordering::SeqCst);
  let second = store.clone().fetch(&Reference::parse("ghcr.io/devcontainers/features/node:2").unwrap()).await.unwrap();
  assert_eq!(second.digest, manifest2_digest);
  assert_eq!(token_requests.load(Ordering::SeqCst), 1, "token exchange must be reused from cache");
  assert_eq!(total_requests.load(Ordering::SeqCst), 2, "cached token must avoid 401 challenge + token exchange");
}
