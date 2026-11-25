//! `actor.image: REF` on a VM driver: the run boots the base VM image with
//! the image's layers above `actor-base` imported and attached.

use std::path::{Path, PathBuf};
use std::time::Duration;

use needy_api_server::control::{Guest, Runs, StopMode};
use needy_core::event::Event;
use needy_core::types::Principal;
use needy_core_actor::image::manifest;
use needy_core_actor::image::store::import::testing::{Compression, Image, Layer, Member};
use oci_registry::testing::TestRegistry;

use crate::testing::{self, new_run};

/// The `actor-base` the stand-in base VM images in these tests record.
pub(crate) const BASE_NAME: &str = "ghcr.io/needy-dev/needy/actor-base:0.0.18-ubuntu-24.04";

/// Records `base` as the `actor-base` the base VM image in `dir` was
/// derived from.
pub(crate) fn record_base(dir: &Path, base: &Image) {
  let mut recorded = manifest::read(dir).unwrap().unwrap();
  recorded.base = Some(base.as_base(BASE_NAME));
  manifest::write(dir, &recorded).unwrap();
}

/// An agent image `FROM` `base` as standard builders push one: a zstd layer
/// with absolute member names and a hard link, empty layers, a gzip layer,
/// its own `PATH` and environment, and an entrypoint a VM ignores.
pub(crate) fn agent(base: &Image) -> Image {
  let mut image = base.from(vec![
    Layer::empty(Compression::Zstd),
    Layer::zstd(&[
      Member::dir("/"),
      Member::dir("/usr/"),
      Member::dir("/usr/bin/"),
      Member::file("/usr/bin/bunzip2", b"bzip2"),
      Member::hard_link("/usr/bin/bzcat", "/usr/bin/bunzip2"),
    ]),
    Layer::empty(Compression::Zstd),
    Layer::gzip(&[
      Member::dir("opt/"),
      Member::dir("opt/agent/"),
      Member::file("opt/agent/agent", b"agent"),
    ]),
  ]);
  image.env = vec![
    "PATH=/opt/agent:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".into(),
    "AGENT_HOME=/opt/agent".into(),
  ];
  image.entrypoint = Some(vec!["/.envbuilder/bin/envbuilder".into()]);
  image
}

fn profile(reference: &str) -> String {
  format!(
    r#"
actor:
  driver: linux-firecracker
  command: [sh, -c, 'echo hi']
  image: {reference}
user: {{ name: agent, uid: 1000, gid: 1000 }}
executors:
  here: {{ driver: native }}
"#
  )
}

async fn events(daemon: &crate::Daemon, run: &str) -> Vec<Event> {
  Runs::events(daemon, run).await.unwrap().events.iter().map(|e| Event::decode(&e.kind, e.version, &e.payload).unwrap()).collect()
}

#[tokio::test]
async fn a_vm_actor_of_an_image_from_actor_base_boots_the_base_with_the_images_layers_attached() {
  let registry = TestRegistry::start().await;
  let base = Image::actor_base();
  agent(&base).push(&registry, "agents/codex", "1");
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  record_base(&started.dir.path().join("image"), &base);
  let reference = registry.repository("agents/codex:1");

  let created = tokio::time::timeout(Duration::from_secs(20), Runs::create(daemon, new_run(&profile(&reference), started.dir.path())))
    .await
    .expect("run create timed out")
    .unwrap();

  let boot = daemon.runs.find(&created.id).unwrap().boot();
  let snapshot = boot.snapshot.clone().expect("the run boots the image's layers");
  assert_eq!(snapshot.layers.len(), 2, "the two layers above actor-base that change files");
  assert_eq!(created.snapshot, Some(snapshot.digest.to_string()));
  assert_eq!(boot.root, Some(needy_core_actor::image::root::stack_digest(BASE_NAME, &base.diff_ids()).unwrap()));

  // The guest stacks them, with the image's PATH and environment.
  let config = Guest::config(daemon, &created.id).await.unwrap();
  assert_eq!(config.layers, snapshot.layers.iter().map(ToString::to_string).collect::<Vec<_>>());
  assert_eq!(config.image_path, "/opt/agent:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin");
  assert_eq!(config.env.get("AGENT_HOME").map(String::as_str), Some("/opt/agent"));

  // The base VM image boots, the layer disks after the root and the guest.
  let run_dir = started.dir.path().join("state/runs").join(&created.id);
  let vm_json: serde_json::Value = serde_json::from_slice(&std::fs::read(run_dir.join("vm.json")).unwrap()).unwrap();
  let drives = vm_json["drives"].as_array().unwrap();
  assert_eq!(PathBuf::from(drives[0]["path_on_host"].as_str().unwrap()), started.dir.path().join("image/rootfs.ext4"));
  assert_eq!(drives.iter().map(|d| d["drive_id"].as_str().unwrap()).collect::<Vec<_>>()[2..], ["layer-0", "layer-1"]);
  for drive in &drives[2..] {
    assert_eq!(&std::fs::read(drive["path_on_host"].as_str().unwrap()).unwrap()[..4], b"hsqs");
  }

  // The trace records the import.
  let imports: Vec<_> = events(daemon, &created.id)
    .await
    .into_iter()
    .filter_map(|e| match e {
      Event::ImageImport(import) => Some(import),
      _ => None,
    })
    .collect();
  assert_eq!(imports.len(), 1);
  assert_eq!(imports[0].reference, reference);
  assert_eq!(imports[0].platform, "linux/amd64");
  assert_eq!((imports[0].pulled, imports[0].skipped), (2, 2));
  assert!(imports[0].warnings.iter().any(|w| w.contains("Entrypoint")), "{:?}", imports[0].warnings);
  assert_eq!(imports[0].error, None);

  Runs::stop(daemon, &created.id, Principal::Operator, StopMode::Now).await.unwrap();
}

#[tokio::test]
async fn a_vm_actor_of_an_image_on_another_base_fails_create_naming_the_base() {
  let registry = TestRegistry::start().await;
  let base = Image::actor_base();
  let mut other = Image::actor_base();
  other.layers[2] = Layer::gzip(&[Member::file(".needy/init", b"another build")]);
  agent(&other).push(&registry, "agents/other", "1");
  let started = testing::daemon().await;
  let daemon = &started.daemon;
  record_base(&started.dir.path().join("image"), &base);

  let refused = Runs::create(daemon, new_run(&profile(&registry.repository("agents/other:1")), started.dir.path())).await.unwrap_err();

  let message = refused.to_string();
  assert!(message.contains("is not built on this needy's actor-base"), "{message}");
  assert!(message.contains(&format!("build the image FROM {BASE_NAME}")), "{message}");
}

#[tokio::test]
async fn a_base_vm_image_that_records_no_actor_base_refuses_an_import() {
  let registry = TestRegistry::start().await;
  agent(&Image::actor_base()).push(&registry, "agents/codex", "1");
  let started = testing::daemon().await;

  let refused =
    Runs::create(&started.daemon, new_run(&profile(&registry.repository("agents/codex:1")), started.dir.path())).await.unwrap_err();

  assert!(refused.to_string().contains("records no actor-base"), "{refused}");
}
