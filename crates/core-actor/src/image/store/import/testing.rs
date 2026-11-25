//! Synthetic OCI images for import tests: a stand-in `actor-base` and images
//! `FROM` it, written straight into an in-process test registry
//! ([`oci_registry::testing::TestRegistry`]) as a builder would push them,
//! with the shapes standard builders produce (gzip and zstd layers, empty
//! layers, absolute member names, a `/` root entry, hard links, a builder's
//! own tree and entrypoint).

use std::collections::BTreeMap;
use std::io::Write as _;

use oci_registry::testing::TestRegistry;
use sha2::Digest as _;

use crate::image::{base, manifest};

/// One member of a layer archive, by its name as written (absolute names
/// and `/` itself included).
#[derive(Debug, Clone)]
pub enum Member {
  Dir(String),
  File(String, Vec<u8>),
  /// A hard link at the first path to the second.
  HardLink(String, String),
  Symlink(String, String),
}

impl Member {
  pub fn dir(path: &str) -> Self {
    Self::Dir(path.into())
  }

  pub fn file(path: &str, content: &[u8]) -> Self {
    Self::File(path.into(), content.to_vec())
  }

  pub fn hard_link(path: &str, target: &str) -> Self {
    Self::HardLink(path.into(), target.into())
  }
}

/// A tar archive of `members`, their names written as given and their
/// times at the epoch, as envbuilder and kaniko write layers.
pub fn tar(members: &[Member]) -> Vec<u8> {
  let mut builder = tar::Builder::new(Vec::new());
  for member in members {
    let (kind, name, data, link): (tar::EntryType, &str, &[u8], Option<&str>) = match member {
      Member::Dir(name) => (tar::EntryType::Directory, name, &[], None),
      Member::File(name, data) => (tar::EntryType::Regular, name, data, None),
      Member::HardLink(name, target) => (tar::EntryType::Link, name, &[], Some(target)),
      Member::Symlink(name, target) => (tar::EntryType::Symlink, name, &[], Some(target)),
    };
    let mut header = tar::Header::new_ustar();
    // NOTICE: the names are written into the header's fields directly:
    // `tar::Header::set_path` refuses absolute names, which builders write.
    let old = header.as_old_mut();
    old.name[..name.len()].copy_from_slice(name.as_bytes());
    if let Some(link) = link {
      old.linkname[..link.len()].copy_from_slice(link.as_bytes());
    }
    header.set_entry_type(kind);
    header.set_mode(if kind == tar::EntryType::Directory {
      0o755
    } else {
      0o644
    });
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header.set_size(data.len() as u64);
    header.set_cksum();
    builder.append(&header, data).expect("append a member");
  }
  builder.into_inner().expect("finish the archive")
}

/// The SHA-256 of `bytes` as `sha256:<hex>`.
pub fn sha256(bytes: &[u8]) -> String {
  let digest: [u8; 32] = sha2::Sha256::digest(bytes).into();
  format!("sha256:{}", digest.iter().map(|b| format!("{b:02x}")).collect::<String>())
}

/// How a layer's blob is compressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
  None,
  Gzip,
  Zstd,
}

/// One layer: its uncompressed archive and how its blob is compressed.
#[derive(Debug, Clone)]
pub struct Layer {
  pub tar: Vec<u8>,
  pub compression: Compression,
}

impl Layer {
  pub fn gzip(members: &[Member]) -> Self {
    Self {
      tar: tar(members),
      compression: Compression::Gzip,
    }
  }

  pub fn zstd(members: &[Member]) -> Self {
    Self {
      tar: tar(members),
      compression: Compression::Zstd,
    }
  }

  pub fn plain(members: &[Member]) -> Self {
    Self {
      tar: tar(members),
      compression: Compression::None,
    }
  }

  /// An empty layer, as a builder writes one for a step that changes no
  /// file: 1024 bytes of zeros.
  pub fn empty(compression: Compression) -> Self {
    Self {
      tar: vec![0; 1024],
      compression,
    }
  }

  /// The layer's DiffID.
  pub fn diff_id(&self) -> String {
    sha256(&self.tar)
  }

  /// The blob and its media type.
  pub fn blob(&self) -> (Vec<u8>, &'static str) {
    match self.compression {
      Compression::None => (self.tar.clone(), "application/vnd.oci.image.layer.v1.tar"),
      Compression::Gzip => {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(&self.tar).expect("gzip a layer");
        (encoder.finish().expect("gzip a layer"), "application/vnd.oci.image.layer.v1.tar+gzip")
      }
      Compression::Zstd => (
        ruzstd::encoding::compress_to_vec(self.tar.as_slice(), ruzstd::encoding::CompressionLevel::Fastest),
        super::IMAGE_LAYER_ZSTD_MEDIA_TYPE,
      ),
    }
  }
}

/// An image: its layers, lowest first, and its config's parts.
#[derive(Debug, Clone)]
pub struct Image {
  pub layers: Vec<Layer>,
  pub labels: BTreeMap<String, String>,
  pub env: Vec<String>,
  pub entrypoint: Option<Vec<String>>,
  /// The OCI architecture (`amd64`, `arm64`).
  pub architecture: String,
  /// Push the manifest as Docker's schema 2 rather than OCI's.
  pub docker_media_types: bool,
}

impl Image {
  /// A stand-in `actor-base`: three layers (a distribution root, a
  /// userland, needy's layer with `/.needy`) and its labels, for `amd64`.
  pub fn actor_base() -> Self {
    Self {
      layers: vec![
        Layer::gzip(&[
          Member::dir("bin/"),
          Member::file("bin/sh", b"distribution shell"),
          Member::dir("etc/"),
          Member::file("etc/os-release", b"ID=ubuntu\n"),
        ]),
        Layer::gzip(&[
          Member::dir("home/"),
          Member::dir("home/agent/"),
          Member::file("etc/passwd", b"agent:x:1000:1000::/home/agent:/bin/sh\n"),
        ]),
        Layer::gzip(&[
          Member::dir(".needy/"),
          Member::file(".needy/init", b"init stub"),
          Member::file(".needy/base-abi", b"1\n"),
        ]),
      ],
      labels: BTreeMap::from([
        (base::LABEL.to_string(), base::ABI.to_string()),
        (crate::image::root::BASE_NAME_LABEL.to_string(), "docker.io/library/ubuntu:24.04".to_string()),
      ]),
      env: vec![format!("PATH={}", needy_core::image::SYSTEM_PATH)],
      entrypoint: None,
      architecture: "amd64".into(),
      docker_media_types: false,
    }
  }

  /// An image `FROM` this one with `layers` above it, as a builder makes
  /// it: this one's layers and config, and `layers`.
  pub fn from(&self, layers: Vec<Layer>) -> Self {
    let mut image = self.clone();
    image.layers.extend(layers);
    image
  }

  /// The base a VM image derived from this image records.
  pub fn as_base(&self, name: &str) -> manifest::Base {
    manifest::Base {
      image: name.into(),
      digest: None,
      diff_ids: self.diff_ids(),
    }
  }

  pub fn diff_ids(&self) -> Vec<String> {
    self.layers.iter().map(Layer::diff_id).collect()
  }

  fn config(&self) -> Vec<u8> {
    let mut config = serde_json::json!({
      "architecture": self.architecture,
      "os": "linux",
      "config": { "Env": self.env, "Labels": self.labels },
      "rootfs": { "type": "layers", "diff_ids": self.diff_ids() },
    });
    if let Some(entrypoint) = &self.entrypoint {
      config["config"]["Entrypoint"] = serde_json::json!(entrypoint);
    }
    serde_json::to_vec(&config).expect("a config serializes")
  }

  /// Writes the image's blobs and manifest into `registry`, tagged
  /// `repository:tag`, and returns the manifest's digest and size.
  pub fn push(&self, registry: &TestRegistry, repository: &str, tag: &str) -> (String, usize) {
    let config = self.config();
    let config_digest = registry.put_blob(&config);
    let layers: Vec<_> = self
      .layers
      .iter()
      .map(|layer| {
        let (blob, media_type) = layer.blob();
        let media_type = match (self.docker_media_types, media_type) {
          (true, "application/vnd.oci.image.layer.v1.tar+gzip") => "application/vnd.docker.image.rootfs.diff.tar.gzip",
          (_, media_type) => media_type,
        };
        serde_json::json!({ "mediaType": media_type, "digest": registry.put_blob(&blob), "size": blob.len() })
      })
      .collect();
    let (manifest_type, config_type) = if self.docker_media_types {
      ("application/vnd.docker.distribution.manifest.v2+json", "application/vnd.docker.container.image.v1+json")
    } else {
      ("application/vnd.oci.image.manifest.v1+json", "application/vnd.oci.image.config.v1+json")
    };
    let manifest = serde_json::json!({
      "schemaVersion": 2,
      "mediaType": manifest_type,
      "config": { "mediaType": config_type, "digest": config_digest, "size": config.len() },
      "layers": layers,
    });
    let bytes = serde_json::to_vec(&manifest).expect("a manifest serializes");
    (registry.put_manifest(repository, tag, manifest_type, &bytes), bytes.len())
  }
}

/// Writes an OCI image index naming `images` by platform (`linux/amd64`),
/// tagged `repository:tag`, as `docker buildx` pushes a multi-platform
/// image, with an attestation manifest of `unknown/unknown` beside them.
pub fn push_index(registry: &TestRegistry, repository: &str, tag: &str, images: &[(&str, &Image)]) -> String {
  let mut manifests = Vec::new();
  for (at, (platform, image)) in images.iter().enumerate() {
    let (digest, size) = image.push(registry, repository, &format!("{tag}-platform-{at}"));
    let (os, architecture) = platform.split_once('/').expect("a platform is OS/ARCH");
    manifests.push(serde_json::json!({
      "mediaType": "application/vnd.oci.image.manifest.v1+json",
      "digest": digest,
      "size": size,
      "platform": { "os": os, "architecture": architecture },
    }));
  }
  manifests.push(serde_json::json!({
    "mediaType": "application/vnd.oci.image.manifest.v1+json",
    "digest": sha256(b"an attestation manifest"),
    "size": 1,
    "platform": { "os": "unknown", "architecture": "unknown" },
  }));
  let index = serde_json::json!({
    "schemaVersion": 2,
    "mediaType": "application/vnd.oci.image.index.v1+json",
    "manifests": manifests,
  });
  let bytes = serde_json::to_vec(&index).expect("an index serializes");
  registry.put_manifest(repository, tag, "application/vnd.oci.image.index.v1+json", &bytes)
}
