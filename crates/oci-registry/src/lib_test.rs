use std::path::Path;
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use oci_client::manifest::OCI_IMAGE_MEDIA_TYPE;

use super::testing::{Auth, ISSUED_TOKEN, TestRegistry};
use super::*;

const REPOSITORY: &str = "needy/probe";
const MANIFEST: &[u8] = br#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{"mediaType":"application/vnd.oci.image.config.v1+json","digest":"sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a","size":2},"layers":[]}"#;

/// Access that reads no system `certs.d` and no user credentials.
fn isolated(docker_config: &Path) -> Access {
  Access::standard().with_cert_roots(Vec::new()).with_docker_config(docker_config).without_proxy()
}

/// A Docker `config.json` in `dir` with `entry` as the `auths` entry of
/// `host`.
fn docker_config(dir: &Path, host: &str, entry: serde_json::Value) {
  let config = serde_json::json!({ "auths": { host: entry } });
  std::fs::write(dir.join("config.json"), config.to_string()).unwrap();
}

fn basic_entry(username: &str, password: &str) -> serde_json::Value {
  serde_json::json!({ "auth": base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}")) })
}

/// Pulls the probe manifest through `access`, as every client does.
async fn pull(access: &Access, registry: &TestRegistry, plain_http: bool) -> std::result::Result<Vec<u8>, String> {
  let connection = access.connect(&registry.host(), plain_http, REPOSITORY, Operation::Pull).await.map_err(|e| e.to_string())?;
  let reference = oci_client::Reference::with_tag(registry.host(), REPOSITORY.to_string(), "latest".to_string());
  connection
    .client
    .pull_manifest_raw(&reference, &connection.auth, &[OCI_IMAGE_MEDIA_TYPE])
    .await
    .map(|(bytes, _)| bytes.to_vec())
    .map_err(|e| format!("{e:?}"))
}

async fn tls_registry(auth: Auth) -> TestRegistry {
  let registry = TestRegistry::start_with(true, auth).await;
  registry.put_manifest(REPOSITORY, "latest", OCI_IMAGE_MEDIA_TYPE, MANIFEST);
  registry
}

/// Records what `tracing` writes while it is alive, on this thread.
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Captured {
  fn text(&self) -> String {
    String::from_utf8_lossy(&self.0.lock().unwrap()).to_string()
  }
}

fn capture() -> (Captured, tracing::subscriber::DefaultGuard) {
  #[derive(Clone)]
  struct Writer(Arc<Mutex<Vec<u8>>>);
  impl std::io::Write for Writer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
      self.0.lock().unwrap().write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
      Ok(())
    }
  }
  let buffer = Arc::new(Mutex::new(Vec::new()));
  let writer = Writer(buffer.clone());
  let subscriber =
    tracing_subscriber::fmt().with_ansi(false).with_max_level(tracing::Level::WARN).with_writer(move || writer.clone()).finish();
  (Captured(buffer), tracing::subscriber::set_default(subscriber))
}

#[test]
fn a_registry_host_is_a_host_and_an_optional_port_only() {
  for good in [
    "registry.example.com",
    "registry.example.com:5000",
    "localhost:5000",
    "127.0.0.1:5000",
  ] {
    assert_eq!(parse_host(good).unwrap(), good);
  }
  for bad in [
    "",
    "https://registry.example.com",
    "registry.example.com/needy",
    "registry",
    "registry .example.com",
  ] {
    assert!(matches!(parse_host(bad), Err(Error::Host { .. })), "{bad:?} parsed");
  }
  assert!(Access::standard().with_insecure_registries(["http://registry.example.com"]).is_err());
}

#[test]
fn the_first_certs_d_root_with_a_directory_for_the_host_wins() {
  let user = tempfile::tempdir().unwrap();
  let system = tempfile::tempdir().unwrap();
  let host = "registry.example.com:5000";
  // The platform's spelling of `host:port` (`hostport` on Windows).
  let dir = certs::host_directories(host).pop().unwrap();
  std::fs::create_dir_all(system.path().join(&dir)).unwrap();
  std::fs::write(system.path().join(&dir).join("system.crt"), "system").unwrap();
  let roots = vec![user.path().to_path_buf(), system.path().to_path_buf()];
  assert_eq!(certs::for_host(&roots, host).unwrap(), [b"system".to_vec()]);
  assert!(certs::for_host(&roots, "other.example.com").unwrap().is_empty());

  std::fs::create_dir_all(user.path().join(&dir)).unwrap();
  for (name, content) in [
    ("b.crt", "b"),
    ("a.crt", "a"),
    ("client.cert", "cert"),
    ("client.key", "key"),
    ("README", "x"),
  ] {
    std::fs::write(user.path().join(&dir).join(name), content).unwrap();
  }
  assert_eq!(certs::for_host(&roots, host).unwrap(), [b"a".to_vec(), b"b".to_vec()], "only the user's CA files, in name order");
}

#[test]
fn a_host_with_a_port_has_containerds_spelling_first_then_the_platforms() {
  let platform = if cfg!(windows) {
    "registry.example.com5000"
  } else {
    "registry.example.com:5000"
  };
  assert_eq!(certs::host_directories("registry.example.com:5000"), ["registry.example.com_5000_", platform]);
  assert_eq!(certs::host_directories("registry.example.com"), ["registry.example.com"]);
}

#[test]
fn a_host_port_underscore_directory_is_read_before_the_colon_one() {
  let root = tempfile::tempdir().unwrap();
  let host = "registry.example.com:5000";
  std::fs::create_dir_all(root.path().join("registry.example.com_5000_")).unwrap();
  std::fs::write(root.path().join("registry.example.com_5000_").join("ca.crt"), "containerd").unwrap();
  let platform = certs::host_directories(host).pop().unwrap();
  std::fs::create_dir_all(root.path().join(&platform)).unwrap();
  std::fs::write(root.path().join(&platform).join("ca.crt"), "docker").unwrap();
  assert_eq!(certs::for_host(&[root.path().to_path_buf()], host).unwrap(), [b"containerd".to_vec()]);
}

#[tokio::test]
async fn a_registry_with_a_private_ca_is_refused_until_certs_d_names_the_ca() {
  let registry = tls_registry(Auth::None).await;
  let config = tempfile::tempdir().unwrap();

  let refused = pull(&isolated(config.path()), &registry, false).await.unwrap_err();
  assert!(refused.to_lowercase().contains("certificate") || refused.contains("UnknownIssuer"), "{refused}");

  let certs_d = tempfile::tempdir().unwrap();
  let access = isolated(config.path()).with_cert_roots(vec![registry.write_certs_d(certs_d.path())]);
  assert_eq!(pull(&access, &registry, false).await.unwrap(), MANIFEST);
}

#[tokio::test]
async fn an_insecure_registry_skips_verification_for_its_host_only_and_warns_each_time() {
  let registry = tls_registry(Auth::None).await;
  let config = tempfile::tempdir().unwrap();

  let other = isolated(config.path()).with_insecure_registries(["127.0.0.1:1"]).unwrap();
  assert!(pull(&other, &registry, false).await.is_err(), "another host's flag does not apply");

  let access = isolated(config.path()).with_insecure_registries([registry.host()]).unwrap();
  let (captured, _guard) = capture();
  assert_eq!(pull(&access, &registry, false).await.unwrap(), MANIFEST);
  assert_eq!(pull(&access, &registry, false).await.unwrap(), MANIFEST);
  let warnings = captured.text();
  let expected = format!("registry {} is an insecure registry", registry.host());
  assert_eq!(warnings.matches(&expected).count(), 2, "{warnings}");
  let connection = access.connect(&registry.host(), false, REPOSITORY, Operation::Pull).await.unwrap();
  assert!(connection.insecure);
}

#[tokio::test]
async fn basic_credentials_answer_a_basic_challenge() {
  // ECR's shape: `/v2/` asks for Basic credentials (`AWS:<token>` from
  // `docker-credential-ecr-login`).
  let registry = tls_registry(Auth::Basic {
    username: "AWS".into(),
    password: "ecr-token".into(),
  })
  .await;
  let config = tempfile::tempdir().unwrap();
  docker_config(config.path(), &registry.host(), basic_entry("AWS", "ecr-token"));
  let certs_d = tempfile::tempdir().unwrap();
  let access = isolated(config.path()).with_cert_roots(vec![registry.write_certs_d(certs_d.path())]);

  assert_eq!(pull(&access, &registry, false).await.unwrap(), MANIFEST);
  let manifest = registry.seen().into_iter().rfind(|seen| seen.path.ends_with("/manifests/latest")).unwrap();
  assert!(manifest.authorization.unwrap().starts_with("Basic "));
}

#[tokio::test]
async fn basic_credentials_get_a_token_from_the_token_service_of_a_bearer_challenge() {
  // Harbor's shape: a robot account's Basic credentials go to the realm,
  // and the registry takes the token it issues.
  let registry = tls_registry(Auth::Token {
    username: "robot$needy".into(),
    password: "robot-secret".into(),
  })
  .await;
  let config = tempfile::tempdir().unwrap();
  docker_config(config.path(), &registry.host(), basic_entry("robot$needy", "robot-secret"));
  let certs_d = tempfile::tempdir().unwrap();
  let access = isolated(config.path()).with_cert_roots(vec![registry.write_certs_d(certs_d.path())]);

  assert_eq!(pull(&access, &registry, false).await.unwrap(), MANIFEST);
  let seen = registry.seen();
  let token = seen.iter().find(|seen| seen.path == "/token").expect("the token service was asked");
  assert!(token.authorization.as_deref().unwrap().starts_with("Basic "), "{token:?}");
  assert!(token.token_request.as_deref().unwrap().contains("scope=repository%3Aneedy%2Fprobe%3Apull"), "{token:?}");
  let manifest = seen.iter().rfind(|seen| seen.path.ends_with("/manifests/latest")).unwrap();
  assert_eq!(manifest.authorization.as_deref(), Some(format!("Bearer {ISSUED_TOKEN}").as_str()));
}

// ROOT CAUSE:
//
// If the Docker credential store held an `identitytoken` (ACR, Docker Hub
// SSO), the feature fetcher sent it to the registry as a Bearer token,
// because `IdentityToken` was mapped to `RegistryAuth::Bearer`, which
// `oci-client` sends as-is; registries refuse a refresh token there.
//
// Before the fix, `/v2/` requests carried `Bearer <refresh token>`.
// The fix exchanges it at the token service (`grant_type=refresh_token`)
// and sends only the access token.
#[tokio::test]
async fn an_identity_token_is_exchanged_for_an_access_token_and_never_sent_to_the_registry() {
  let refresh = "identity-refresh-token";
  let registry = tls_registry(Auth::RefreshToken {
    refresh_token: refresh.into(),
  })
  .await;
  let config = tempfile::tempdir().unwrap();
  docker_config(config.path(), &registry.host(), serde_json::json!({ "identitytoken": refresh }));
  let certs_d = tempfile::tempdir().unwrap();
  let access = isolated(config.path()).with_cert_roots(vec![registry.write_certs_d(certs_d.path())]);

  let connection = access.connect(&registry.host(), false, REPOSITORY, Operation::Push).await.unwrap();
  assert!(matches!(&connection.auth, RegistryAuth::Bearer(token) if token == ISSUED_TOKEN));
  // The token service says the token lasts 300 seconds; the connection is
  // reused for less than that.
  assert!(connection.is_fresh());
  assert!(connection.reuse_until <= std::time::Instant::now() + std::time::Duration::from_secs(290));
  assert_eq!(pull(&access, &registry, false).await.unwrap(), MANIFEST);

  let seen = registry.seen();
  let exchanges: Vec<_> = seen.iter().filter(|seen| seen.path == "/token").collect();
  assert_eq!(exchanges.len(), 2, "one exchange per connection: {seen:?}");
  for exchange in &exchanges {
    assert_eq!(exchange.method, "POST");
    let form = exchange.token_request.as_deref().unwrap();
    assert!(form.contains("grant_type=refresh_token"), "{form}");
    assert!(form.contains(&format!("refresh_token={refresh}")), "{form}");
    assert!(form.contains("client_id=needy"), "{form}");
    assert!(form.contains("service=test-registry"), "{form}");
  }
  assert!(exchanges[0].token_request.as_deref().unwrap().contains("scope=repository%3Aneedy%2Fprobe%3Apull%2Cpush"));
  for request in seen.iter().filter(|seen| seen.path.starts_with("/v2")) {
    assert!(!request.authorization.as_deref().unwrap_or("").contains(refresh), "the refresh token reached {request:?}");
  }
}

#[tokio::test]
async fn an_identity_token_for_a_registry_without_a_token_service_is_a_typed_error() {
  let refresh = "identity-refresh-token";
  let registry = tls_registry(Auth::Basic {
    username: "user".into(),
    password: "password".into(),
  })
  .await;
  let config = tempfile::tempdir().unwrap();
  docker_config(config.path(), &registry.host(), serde_json::json!({ "identitytoken": refresh }));
  let certs_d = tempfile::tempdir().unwrap();
  let access = isolated(config.path()).with_cert_roots(vec![registry.write_certs_d(certs_d.path())]);

  let error = access.connect(&registry.host(), false, REPOSITORY, Operation::Pull).await.err().unwrap();
  assert!(matches!(&error, Error::IdentityToken { host, .. } if *host == registry.host()), "{error}");
  assert!(registry.seen().iter().all(|seen| seen.authorization.is_none()), "{:?}", registry.seen());
}

#[tokio::test]
async fn a_plain_http_registry_is_never_sent_credentials() {
  let registry = TestRegistry::start_with(
    false,
    Auth::Basic {
      username: "user".into(),
      password: "password".into(),
    },
  )
  .await;
  registry.put_manifest(REPOSITORY, "latest", OCI_IMAGE_MEDIA_TYPE, MANIFEST);
  let config = tempfile::tempdir().unwrap();
  docker_config(config.path(), &registry.host(), basic_entry("user", "password"));
  let access = isolated(config.path()).with_insecure_registries([registry.host()]).unwrap();

  let connection = access.connect(&registry.host(), true, REPOSITORY, Operation::Push).await.unwrap();
  assert!(matches!(connection.auth, RegistryAuth::Anonymous));
  assert!(!connection.insecure, "plain HTTP is not an insecure TLS registry");
  assert!(pull(&access, &registry, true).await.is_err(), "the registry refuses anonymous access");
  let seen = registry.seen();
  assert!(!seen.is_empty());
  assert!(seen.iter().all(|seen| seen.authorization.is_none()), "{seen:?}");
}
