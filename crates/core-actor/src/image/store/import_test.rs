use std::path::Path;

use oci_registry::testing::{Auth, TestRegistry};

use super::testing::{Compression, Image, Layer, Member, push_index};
use super::*;

const BASE_NAME: &str = "ghcr.io/needy-dev/needy/actor-base:0.0.18-ubuntu-24.04";

fn store(dir: &Path) -> Store {
  Store::new(dir.join("cache"), dir.join("data"))
}

fn access() -> Access {
  Access::standard().with_cert_roots(Vec::new()).with_docker_config("/nonexistent").without_proxy()
}

fn reference(registry: &TestRegistry, path: &str) -> Reference {
  Reference::parse(&registry.repository(path)).unwrap()
}

/// The member names of stored layer `digest`, in the store's canonical form.
fn entries(store: &Store, digest: &Digest) -> Vec<String> {
  let mut archive = tar::Archive::new(store.open_layer(digest).unwrap());
  archive.entries().unwrap().map(|entry| entry.unwrap().path().unwrap().display().to_string()).collect()
}

/// An agent image as envbuilder writes one: zstd layers, empty layers
/// between them (the same DiffID again and again), absolute member names
/// with a `/` root entry and epoch times, a hard link, its own
/// `/.envbuilder` tree and entrypoint.
fn envbuilder_agent(base: &Image) -> Image {
  let mut image = base.from(vec![
    Layer::empty(Compression::Zstd),
    Layer::empty(Compression::Zstd),
    Layer::zstd(&[
      Member::dir("/"),
      Member::dir("/usr/"),
      Member::dir("/usr/bin/"),
      Member::file("/usr/bin/bunzip2", b"bzip2 program"),
      Member::hard_link("/usr/bin/bzcat", "/usr/bin/bunzip2"),
      Member::file("/etc/passwd", b"agent:x:1000:1000::/home/agent:/bin/bash\n"),
    ]),
    Layer::empty(Compression::Zstd),
    Layer::gzip(&[
      Member::dir("opt/"),
      Member::dir("opt/codex/"),
      Member::file("opt/codex/codex", b"codex program"),
    ]),
    Layer::zstd(&[
      Member::dir("/.envbuilder/"),
      Member::file("/.envbuilder/bin/envbuilder", b"envbuilder program"),
    ]),
    Layer::empty(Compression::Zstd),
  ]);
  image.env = vec![
    "PATH=/usr/local/python/current/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".into(),
    "PYTHON_PATH=/usr/local/python/current".into(),
  ];
  image.entrypoint = Some(vec!["/.envbuilder/bin/envbuilder".into()]);
  image
}

#[tokio::test]
async fn an_image_from_actor_base_imports_only_its_layers_above_the_base_and_they_make_layer_disks() {
  let registry = TestRegistry::start().await;
  let base = Image::actor_base();
  let image = envbuilder_agent(&base);
  image.push(&registry, "agents/codex", "1");
  let dir = tempfile::tempdir().unwrap();
  let store = store(dir.path());

  let imported = import(&store, &access(), &reference(&registry, "agents/codex:1"), &base.as_base(BASE_NAME), "x86_64").await.unwrap();

  assert_eq!(imported.platform, "linux/amd64");
  assert_eq!(imported.root, root::stack_digest(BASE_NAME, &base.diff_ids()).unwrap());
  assert_eq!(imported.layers.len(), 3, "the three layers above the base that change files: {imported:?}");
  assert_eq!((imported.pulled, imported.skipped), (3, 4));
  // The base's blobs are never fetched.
  for layer in &base.layers {
    let blob = super::testing::sha256(&layer.blob().0);
    assert!(!registry.requests().iter().any(|r| r.ends_with(&blob)), "fetched a base layer: {:?}", registry.requests());
  }
  // Members are kept relative, without the root entry, hard links included.
  assert_eq!(
    entries(&store, &imported.layers[0]),
    [
      "usr",
      "usr/bin",
      "usr/bin/bunzip2",
      "usr/bin/bzcat",
      "etc/passwd"
    ]
  );
  assert_eq!(imported.path.as_deref(), Some("/usr/local/python/current/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"));
  assert_eq!(imported.env, BTreeMap::from([("PYTHON_PATH".to_string(), "/usr/local/python/current".to_string())]));
  assert!(imported.warnings.iter().any(|w| w.contains("/.envbuilder") && w.contains("envbuilder")), "{:?}", imported.warnings);
  assert!(imported.warnings.iter().any(|w| w.contains("Entrypoint") && w.contains("ignored")), "{:?}", imported.warnings);

  // They stack and make the disks a VM driver attaches.
  let snapshot = store.snapshot(&imported.layers).unwrap();
  let disks = store.materialize(&snapshot).unwrap();
  assert_eq!(disks.iter().map(|d| d.digest).collect::<Vec<_>>(), imported.layers);
  for disk in &disks {
    assert_eq!(&std::fs::read(&disk.path).unwrap()[..4], b"hsqs");
  }

  // A second import finds every layer stored by its DiffID and pulls none.
  let before = registry.requests().len();
  let again = import(&store, &access(), &reference(&registry, "agents/codex:1"), &base.as_base(BASE_NAME), "x86_64").await.unwrap();
  assert_eq!(again.layers, imported.layers);
  assert_eq!(again.pulled, 0);
  let blobs = registry.requests()[before..].iter().filter(|r| r.contains("/blobs/")).count();
  assert_eq!(blobs, 1, "only the config: {:?}", &registry.requests()[before..]);
}

#[tokio::test]
async fn an_index_is_resolved_for_the_actors_platform_and_one_without_it_is_refused() {
  let registry = TestRegistry::start().await;
  let base = Image::actor_base();
  let amd64 = base.from(vec![Layer::gzip(&[Member::file("amd64", b"x86")])]);
  let mut arm_base = Image::actor_base();
  arm_base.architecture = "arm64".into();
  let arm64 = arm_base.from(vec![Layer::gzip(&[Member::file("arm64", b"arm")])]);
  let index = push_index(&registry, "agents/multi", "1", &[("linux/arm64", &arm64), ("linux/amd64", &amd64)]);
  let dir = tempfile::tempdir().unwrap();
  let store = store(dir.path());

  let imported = import(&store, &access(), &reference(&registry, "agents/multi:1"), &base.as_base(BASE_NAME), "x86_64").await.unwrap();
  assert_eq!(entries(&store, &imported.layers[0]), ["amd64"]);
  // By the index's digest too.
  let pinned = Reference::parse(&registry.repository(&format!("agents/multi@{index}"))).unwrap();
  let by_digest = import(&store, &access(), &pinned, &base.as_base(BASE_NAME), "x86_64").await.unwrap();
  assert_eq!(by_digest.layers, imported.layers);

  let only_arm = push_index(&registry, "agents/arm", "1", &[("linux/arm64", &arm64)]);
  let _ = only_arm;
  let refused = import(&store, &access(), &reference(&registry, "agents/arm:1"), &base.as_base(BASE_NAME), "x86_64").await.unwrap_err();
  assert!(matches!(refused, Error::Platform { .. }), "{refused}");
  assert_eq!(refused.to_string(), format!("{} has no image for linux/amd64; it has linux/arm64", registry.repository("agents/arm:1")));
}

#[tokio::test]
async fn an_image_on_another_base_is_refused_naming_the_base_it_needs() {
  let registry = TestRegistry::start().await;
  let base = Image::actor_base();
  let mut other = Image::actor_base();
  other.layers[2] = Layer::gzip(&[
    Member::dir(".needy/"),
    Member::file(".needy/init", b"another build of the stubs"),
  ]);
  other.labels.insert(root::BASE_NAME_LABEL.into(), "docker.io/library/debian:13".into());
  other.from(vec![Layer::gzip(&[Member::file("agent", b"x")])]).push(&registry, "agents/other", "1");
  let dir = tempfile::tempdir().unwrap();
  let store = store(dir.path());
  let mut recorded = base.as_base(BASE_NAME);
  recorded.digest = Some(super::testing::sha256(b"the base's manifest"));

  let refused = import(&store, &access(), &reference(&registry, "agents/other:1"), &recorded, "x86_64").await.unwrap_err();

  let message = refused.to_string();
  assert!(matches!(refused, Error::Base { .. }), "{message}");
  assert!(message.contains("is not built on this needy's actor-base: its layer 2 is "), "{message}");
  assert!(message.contains("(its org.opencontainers.image.base.name label names docker.io/library/debian:13)"), "{message}");
  assert!(
    message
      .ends_with(&format!("build the image FROM ghcr.io/needy-dev/needy/actor-base@{} ({BASE_NAME})", recorded.digest.as_deref().unwrap())),
    "{message}"
  );
  // Nothing was pulled.
  assert!(!dir.path().join("cache/layers").exists());

  // An image with fewer layers than the base is not on it either.
  Image {
    layers: base.layers[..1].to_vec(),
    ..base.clone()
  }
  .push(&registry, "agents/short", "1");
  let refused = import(&store, &access(), &reference(&registry, "agents/short:1"), &recorded, "x86_64").await.unwrap_err();
  assert!(refused.to_string().contains("it has 1 layers, and the base has 3"), "{refused}");
}

#[tokio::test]
async fn an_image_of_another_base_abi_is_refused() {
  let registry = TestRegistry::start().await;
  let base = Image::actor_base();
  let mut image = base.from(vec![Layer::gzip(&[Member::file("agent", b"x")])]);
  image.labels.insert(base::LABEL.into(), "999".into());
  image.push(&registry, "agents/abi", "1");
  let mut unlabelled = image.clone();
  unlabelled.labels.remove(base::LABEL);
  unlabelled.push(&registry, "agents/abi", "unlabelled");
  let dir = tempfile::tempdir().unwrap();
  let store = store(dir.path());

  let refused = import(&store, &access(), &reference(&registry, "agents/abi:1"), &base.as_base(BASE_NAME), "x86_64").await.unwrap_err();
  assert!(matches!(refused, Error::Abi(_)), "{refused}");
  assert!(refused.to_string().contains(&format!("has base ABI 999, and this needy speaks base ABI {}", base::ABI)), "{refused}");
  assert!(refused.to_string().contains(&format!("rebuild it FROM {BASE_NAME}")), "{refused}");

  let refused =
    import(&store, &access(), &reference(&registry, "agents/abi:unlabelled"), &base.as_base(BASE_NAME), "x86_64").await.unwrap_err();
  assert!(refused.to_string().contains("records no base ABI"), "{refused}");
}

#[tokio::test]
async fn a_layer_that_writes_removes_or_hides_needys_paths_is_refused_and_not_kept() {
  let registry = TestRegistry::start().await;
  let base = Image::actor_base();
  let cases = [
    ("writes", vec![Member::file("/.needy/init", b"another init")], "layer {} writes /.needy/init"),
    ("removes", vec![Member::file(".wh..needy", b"")], "layer {} removes /.needy"),
    ("hides", vec![Member::file("./.wh..wh..opq", b"")], "hides everything below it, /.needy included"),
  ];
  for (tag, members, expected) in cases {
    let layer = Layer::gzip(&members);
    let diff_id = layer.diff_id();
    base.from(vec![Layer::gzip(&[Member::file("fine", b"x")]), layer]).push(&registry, "agents/needy", tag);
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());

    let refused = import(&store, &access(), &reference(&registry, &format!("agents/needy:{tag}")), &base.as_base(BASE_NAME), "x86_64")
      .await
      .unwrap_err();

    assert!(matches!(refused, Error::NeedyPath { .. }), "{refused}");
    assert!(refused.to_string().contains(&expected.replace("{}", &diff_id)), "{tag}: {refused}");
    assert!(store.imported(&Digest::parse(&diff_id).unwrap()).is_none(), "{tag}: the refused layer was kept");
  }
}

// An image `FROM needy-actor` (`actor-base` and the container shim step)
// is refused with the base to build FROM instead, rather than as a layer
// that happens to write `/.needy`.
#[tokio::test]
async fn an_image_from_the_container_base_is_refused_with_the_base_to_build_from() {
  let registry = TestRegistry::start().await;
  let base = Image::actor_base();
  let mut needy_actor = base.from(vec![Layer::gzip(&[
    Member::dir(".needy/real/"),
    Member::dir(".needy/real/usr/bin/"),
    Member::file(".needy/real/usr/bin/bash", b"the real bash"),
    Member::hard_link("usr/bin/bash", ".needy/proxy"),
  ])]);
  needy_actor.labels.insert(root::BASE_NAME_LABEL.into(), String::new());
  needy_actor.entrypoint = Some(vec!["/.needy/init".into()]);
  let shim = needy_actor.layers[3].diff_id();
  needy_actor.from(vec![Layer::gzip(&[Member::file("agent", b"x")])]).push(&registry, "agents/old", "1");
  let dir = tempfile::tempdir().unwrap();
  let store = store(dir.path());

  let refused = import(&store, &access(), &reference(&registry, "agents/old:1"), &base.as_base(BASE_NAME), "x86_64").await.unwrap_err();

  assert!(matches!(refused, Error::ContainerBase { .. }), "{refused}");
  assert_eq!(
    refused.to_string(),
    format!(
      "{} is built FROM needy-actor, the container base, whose shim layer (layer {shim}) rewrites the shells under /.needy; a VM \
       actor needs no shims, so build the image FROM {BASE_NAME} instead",
      registry.repository("agents/old:1")
    )
  );
}

#[tokio::test]
async fn a_blob_that_decompresses_to_another_layer_than_its_config_names_is_refused_and_not_kept() {
  let registry = TestRegistry::start().await;
  let base = Image::actor_base();
  // The manifest names one layer's blob, and the config another layer's
  // DiffID: the blob passes the registry's digest check and decompresses
  // to a layer the config does not name.
  let layer = Layer::zstd(&[Member::file("agent", b"x")]);
  let lying = base.from(vec![Layer::zstd(&[Member::file("agent", b"y")])]);
  // Push an image whose config names the second layer's DiffID but whose
  // manifest names the first layer's blob.
  let (blob, media_type) = layer.blob();
  let blob_digest = registry.put_blob(&blob);
  let config = serde_json::json!({
    "architecture": "amd64", "os": "linux",
    "config": { "Labels": base.labels },
    "rootfs": { "type": "layers", "diff_ids": lying.diff_ids() },
  });
  let config = serde_json::to_vec(&config).unwrap();
  let config_digest = registry.put_blob(&config);
  let mut layers: Vec<_> = base
    .layers
    .iter()
    .map(|l| {
      let (b, t) = l.blob();
      serde_json::json!({ "mediaType": t, "digest": registry.put_blob(&b), "size": b.len() })
    })
    .collect();
  layers.push(serde_json::json!({ "mediaType": media_type, "digest": blob_digest, "size": blob.len() }));
  let manifest = serde_json::json!({
    "schemaVersion": 2, "mediaType": "application/vnd.oci.image.manifest.v1+json",
    "config": { "mediaType": "application/vnd.oci.image.config.v1+json", "digest": config_digest, "size": config.len() },
    "layers": layers,
  });
  registry.put_manifest("agents/tampered", "lying", "application/vnd.oci.image.manifest.v1+json", &serde_json::to_vec(&manifest).unwrap());
  let dir = tempfile::tempdir().unwrap();
  let store = store(dir.path());

  let refused =
    import(&store, &access(), &reference(&registry, "agents/tampered:lying"), &base.as_base(BASE_NAME), "x86_64").await.unwrap_err();

  assert!(matches!(refused, Error::DiffId { .. }), "{refused}");
  assert!(!dir.path().join("cache/layers").exists() || std::fs::read_dir(dir.path().join("cache/layers")).unwrap().count() == 0);
}

#[tokio::test]
async fn a_layer_repeated_in_a_row_is_stacked_once_and_one_that_comes_back_later_is_refused() {
  let registry = TestRegistry::start().await;
  let base = Image::actor_base();
  let one = Layer::gzip(&[Member::file("one", b"1")]);
  let two = Layer::gzip(&[Member::file("two", b"2")]);
  base.from(vec![one.clone(), one.clone(), two.clone()]).push(&registry, "agents/repeat", "in-a-row");
  base.from(vec![one.clone(), two, one]).push(&registry, "agents/repeat", "later");
  let dir = tempfile::tempdir().unwrap();
  let store = store(dir.path());

  let imported =
    import(&store, &access(), &reference(&registry, "agents/repeat:in-a-row"), &base.as_base(BASE_NAME), "x86_64").await.unwrap();
  assert_eq!((imported.layers.len(), imported.skipped), (2, 1));

  let refused =
    import(&store, &access(), &reference(&registry, "agents/repeat:later"), &base.as_base(BASE_NAME), "x86_64").await.unwrap_err();
  assert!(refused.to_string().contains("comes back at layer 5, after other layers"), "{refused}");
}

#[tokio::test]
async fn a_hard_link_to_a_file_outside_its_layer_is_refused() {
  let registry = TestRegistry::start().await;
  let base = Image::actor_base();
  base.from(vec![Layer::gzip(&[Member::hard_link("bin/sh2", "bin/sh")])]).push(&registry, "agents/link", "1");
  let dir = tempfile::tempdir().unwrap();

  let refused =
    import(&store(dir.path()), &access(), &reference(&registry, "agents/link:1"), &base.as_base(BASE_NAME), "x86_64").await.unwrap_err();

  assert!(refused.to_string().contains("has a hard link /bin/sh2 to /bin/sh, which is not a file of the same layer"), "{refused}");
}

// Docker's own media types (`docker build` with the classic builder, `docker
// push` of an engine image) and uncompressed layers import as OCI's do.
#[tokio::test]
async fn docker_schema_2_manifests_and_uncompressed_layers_import() {
  let registry = TestRegistry::start().await;
  let mut base = Image::actor_base();
  base.docker_media_types = true;
  base
    .from(vec![
      Layer::gzip(&[Member::file("docker", b"d")]),
      Layer::plain(&[Member::file("plain", b"p")]),
    ])
    .push(&registry, "agents/docker", "1");
  let dir = tempfile::tempdir().unwrap();
  let store = store(dir.path());

  let imported = import(&store, &access(), &reference(&registry, "agents/docker:1"), &base.as_base(BASE_NAME), "x86_64").await.unwrap();

  assert_eq!(imported.layers.iter().map(|l| entries(&store, l)).collect::<Vec<_>>(), [["docker"], ["plain"]]);
}

// A registry over TLS with a private CA and credentials, as a CI's or a
// cluster's registry is: `certs.d` and the Docker credential store.
#[tokio::test]
async fn an_import_reaches_a_tls_registry_with_credentials() {
  use base64::Engine as _;

  let registry = TestRegistry::start_with(
    true,
    Auth::Basic {
      username: "ci".into(),
      password: "secret".into(),
    },
  )
  .await;
  let base = Image::actor_base();
  base.from(vec![Layer::zstd(&[Member::file("agent", b"x")])]).push(&registry, "agents/private", "1");
  let certs_d = tempfile::tempdir().unwrap();
  let config = tempfile::tempdir().unwrap();
  let auth = base64::engine::general_purpose::STANDARD.encode("ci:secret");
  std::fs::write(config.path().join("config.json"), serde_json::json!({ "auths": { registry.host(): { "auth": auth } } }).to_string())
    .unwrap();
  let access =
    Access::standard().with_cert_roots(vec![registry.write_certs_d(certs_d.path())]).with_docker_config(config.path()).without_proxy();
  let dir = tempfile::tempdir().unwrap();

  let imported =
    import(&store(dir.path()), &access, &reference(&registry, "agents/private:1"), &base.as_base(BASE_NAME), "x86_64").await.unwrap();

  assert_eq!(imported.layers.len(), 1);
}

#[test]
fn a_reference_is_an_oci_reference_or_a_plain_http_one() {
  let plain = Reference::parse("http://127.0.0.1:5000/agents/codex:1").unwrap();
  assert!(plain.plain_http);
  assert_eq!(plain.registry(), "127.0.0.1:5000");
  let short = Reference::parse("needy-agents").unwrap();
  assert!(!short.plain_http);
  assert_eq!(short.registry(), "docker.io");
  assert!(Reference::parse("https://registry.example.com/agent:1").is_err());
  assert!(Reference::parse("Not A Reference").is_err());
}
