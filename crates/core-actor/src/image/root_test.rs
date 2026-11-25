use std::collections::HashMap;

use needy_config::profile::ActorDriver;

use super::*;
use crate::image::manifest::{Base, Source};

fn diff_id(layer: &[u8]) -> String {
  Digest::of(layer).to_string()
}

/// `actor-base`'s layers: the distribution's, the userland, the needy layer.
fn base_layers() -> Vec<String> {
  vec![
    diff_id(b"distribution"),
    diff_id(b"userland"),
    diff_id(b"needy"),
  ]
}

/// A release's VM image manifest, with `base` when given.
fn manifest(base: Option<Vec<String>>) -> Manifest {
  Manifest {
    version: "0.0.18".into(),
    driver: ActorDriver::LinuxFirecracker,
    arch: "x86_64".into(),
    base_abi: 1,
    source: Some(Source::Release {
      tag: "v0.0.18".into(),
      url: "https://example.com/needy-actor-linux-firecracker-x86_64.tar.gz".into(),
    }),
    sha256: Some("0".repeat(64)),
    base: base.map(|diff_ids| Base {
      image: "ghcr.io/needy-dev/needy/actor-base:v0.0.18-ubuntu-24.04".into(),
      digest: None,
      diff_ids,
    }),
  }
}

/// A container image with `layers` and the labels `labels`.
fn container(layers: Vec<String>, labels: &[(&str, &str)]) -> engine::Image {
  engine::Image {
    id: format!("sha256:{}", "e".repeat(64)),
    os: "linux".into(),
    architecture: "amd64".into(),
    labels: labels.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<HashMap<_, _>>(),
    diff_ids: layers,
  }
}

/// `needy-actor`: `actor-base`'s layers and the shim layer.
fn needy_actor(base_name: &str) -> engine::Image {
  let mut layers = base_layers();
  layers.push(diff_id(b"shims"));
  container(layers, &[(BASE_NAME_LABEL, base_name), ("needy.base-abi", "1")])
}

#[test]
fn the_stack_digest_is_the_chain_id_of_the_layers() {
  // sha256 over "<DiffID of a> <DiffID of b>", computed outside Rust:
  //   printf '%s %s' "sha256:$(printf a | shasum -a 256 | cut -d' ' -f1)" \
  //     "sha256:$(printf b | shasum -a 256 | cut -d' ' -f1)" | shasum -a 256
  let digest = stack_digest("actor-base", &[diff_id(b"a"), diff_id(b"b")]).unwrap();
  assert_eq!(digest.to_string(), "sha256:51c0c8ace48498d6f5fee6b0592cc06f2da0f3cbe09c5a34a97dce85c3889676");
}

#[test]
fn a_stack_without_layers_or_with_an_invalid_one_has_no_digest() {
  assert!(matches!(stack_digest("actor-base", &[]), Err(Error::NoLayers { .. })));
  let invalid = stack_digest("actor-base", &[diff_id(b"a"), "sha256:abc".into()]).unwrap_err();
  assert!(matches!(&invalid, Error::InvalidLayer { diff_id, .. } if diff_id == "sha256:abc"), "{invalid}");
}

#[test]
fn a_vm_image_with_a_base_is_rooted_on_the_bases_stack_digest_not_its_archive() {
  let root = of_vm(&manifest(Some(base_layers()))).unwrap();
  assert_eq!(root, Some(stack_digest("actor-base", &base_layers()).unwrap()));
}

#[test]
fn a_vm_image_without_a_base_keeps_its_archive_digest_or_none() {
  let released = manifest(None);
  assert_eq!(of_vm(&released).unwrap(), Some(Digest::parse(&format!("sha256:{}", "0".repeat(64))).unwrap()));

  let built = Manifest {
    sha256: None,
    ..released
  };
  assert_eq!(of_vm(&built).unwrap(), None, "the caller hashes rootfs.ext4");
}

#[test]
fn a_vm_image_whose_base_records_no_layers_is_refused() {
  assert!(matches!(of_vm(&manifest(Some(Vec::new()))), Err(Error::NoLayers { .. })));
}

#[test]
fn needy_actor_is_rooted_on_its_layers_without_the_shim_layer() {
  let on_the_base = stack_digest("actor-base", &base_layers()).unwrap();
  // Built from a checkout (an empty label) and by the release.
  assert_eq!(of_container("needy-actor:latest", &needy_actor("")).unwrap(), on_the_base);
  assert_eq!(
    of_container("ghcr.io/needy-dev/needy-actor:v0.0.18", &needy_actor("ghcr.io/needy-dev/needy/actor-base:v0.0.18-ubuntu-24.04")).unwrap(),
    on_the_base
  );
  // The same root as the VM image derived from the same `actor-base`.
  assert_eq!(of_vm(&manifest(Some(base_layers()))).unwrap(), Some(on_the_base));
}

#[test]
fn a_container_image_whose_base_layers_cannot_be_told_is_refused() {
  // `actor-base` itself, tagged as `needy-actor`: its label names the
  // distribution, and dropping its top layer would drop the needy layer.
  let actor_base = container(base_layers(), &[(BASE_NAME_LABEL, "docker.io/library/ubuntu:24.04")]);
  let refused = of_container("needy-actor:latest", &actor_base).unwrap_err();
  assert!(refused.to_string().contains("names docker.io/library/ubuntu:24.04, not an actor-base image"), "{refused}");

  // One layer: no base under the shim layer.
  let flat = container(vec![diff_id(b"everything")], &[(BASE_NAME_LABEL, "")]);
  let refused = of_container("needy-actor:latest", &flat).unwrap_err();
  assert!(refused.to_string().contains("it has 1 layers"), "{refused}");
  assert!(refused.to_string().contains("needy image build --linux-container"), "{refused}");
}

#[test]
fn a_container_image_that_records_no_base_keeps_its_id() {
  let id = Digest::parse(&format!("sha256:{}", "e".repeat(64))).unwrap();
  // A `needy-actor` from before B-1: one layer, no base name label.
  let flattened = container(vec![diff_id(b"everything")], &[("needy.base-abi", "1")]);
  assert_eq!(of_container("needy-actor:latest", &flattened).unwrap(), id);
  // A custom root, even one `FROM needy-actor` that inherits its labels.
  assert_eq!(of_container("debian:bookworm-slim", &container(vec![diff_id(b"debian")], &[])).unwrap(), id);
  assert_eq!(of_container("example.com/custom-actor:1", &needy_actor("")).unwrap(), id);
}
