//! Image definitions: what an actor runs on, written as a root and an
//! ordered list of layers, either in an image file (its own document kind,
//! with `extends`) or inline in a profile's `actor.image`. See the images
//! and run data spec,
//! `docs/ai/references/architecture/2026-11-17-images-and-run-data-spec.md`.
//!
//! Building a definition is not this crate's job; `needy-core` plans it.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde::de::{self, MapAccess, Visitor};
use serde_yaml::Value;

use crate::{Error, Kind, Source, merge, source};

/// What `actor.image` names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Image {
  /// An OCI image reference, run or booted as it is: the container drivers
  /// run it, the VM drivers boot the VM image `needy image build --from`
  /// made of it.
  Reference(String),
  /// An image definition, from an image file or written inline.
  Definition(Definition),
}

/// An image definition: a root and the layers built over it, in order.
// TODO(images): `root` takes an image reference only; the Dockerfile build
// form (`build:`, as executors' `container.build` has it) and the default
// root of a definition without `root:` come with slice 5 of the images and
// run data spec.
// TODO(images): `hooks` (`start`, `stop`, appending along `extends`) come
// with slice 4 of the spec; until then `hooks` is an unknown key.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Definition {
  /// The OCI image the layers are built over.
  pub root: Option<String>,
  /// Built in order over the root. Along `extends` the parent's come first.
  pub layers: Vec<Layer>,
  /// The environment of the image's programs.
  pub env: BTreeMap<String, String>,
}

/// One entry of a definition's `layers`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "RawLayer")]
pub enum Layer {
  /// A shell script run by `sh -c` as root, like a Dockerfile `RUN`.
  Run(String),
  /// A Dev Container feature by reference (OCI reference, `https:` tarball, or
  /// local directory), with optional `with:` option values.
  Feature {
    /// The feature reference as written (e.g. `ghcr.io/devcontainers/features/node:1`).
    reference: String,
    /// Option values passed under `with:`.
    options: BTreeMap<String, FeatureOption>,
  },
}

/// A Dev Container feature option value under `with:` (string, boolean, or
/// integer).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeatureOption {
  String(String),
  Bool(bool),
  Int(i64),
}

impl fmt::Display for FeatureOption {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::String(value) => f.write_str(value),
      Self::Bool(value) => write!(f, "{value}"),
      Self::Int(value) => write!(f, "{value}"),
    }
  }
}

impl From<&str> for FeatureOption {
  fn from(value: &str) -> Self {
    Self::String(value.to_string())
  }
}

impl From<String> for FeatureOption {
  fn from(value: String) -> Self {
    Self::String(value)
  }
}

impl From<bool> for FeatureOption {
  fn from(value: bool) -> Self {
    Self::Bool(value)
  }
}

impl From<i64> for FeatureOption {
  fn from(value: i64) -> Self {
    Self::Int(value)
  }
}

impl serde::Serialize for FeatureOption {
  fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
    match self {
      Self::String(value) => serializer.serialize_str(value),
      Self::Bool(value) => serializer.serialize_bool(*value),
      Self::Int(value) => serializer.serialize_i64(*value),
    }
  }
}

impl<'de> Deserialize<'de> for FeatureOption {
  fn deserialize<D: de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
    deserializer.deserialize_any(FeatureOptionVisitor)
  }
}

struct FeatureOptionVisitor;

impl<'de> Visitor<'de> for FeatureOptionVisitor {
  type Value = FeatureOption;

  fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
    f.write_str("a string, boolean, or integer feature option value")
  }

  fn visit_str<E: de::Error>(self, value: &str) -> Result<FeatureOption, E> {
    Ok(FeatureOption::String(value.to_string()))
  }

  fn visit_string<E: de::Error>(self, value: String) -> Result<FeatureOption, E> {
    Ok(FeatureOption::String(value))
  }

  fn visit_bool<E: de::Error>(self, value: bool) -> Result<FeatureOption, E> {
    Ok(FeatureOption::Bool(value))
  }

  fn visit_i64<E: de::Error>(self, value: i64) -> Result<FeatureOption, E> {
    Ok(FeatureOption::Int(value))
  }

  fn visit_u64<E: de::Error>(self, value: u64) -> Result<FeatureOption, E> {
    i64::try_from(value).map(FeatureOption::Int).map_err(|_| E::custom(format!("feature option integer {value} is out of range")))
  }

  fn visit_f64<E: de::Error>(self, value: f64) -> Result<FeatureOption, E> {
    Err(E::custom(format!("feature option value {value} is a floating-point number; quote decimal versions as strings (e.g. \"{value}\")")))
  }
}

/// A layer as written, before its kind is decided.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLayer {
  #[serde(default)]
  run: Option<String>,
  #[serde(default)]
  feature: Option<String>,
  /// A feature's options.
  #[serde(default)]
  with: Option<BTreeMap<String, FeatureOption>>,
}

impl TryFrom<RawLayer> for Layer {
  type Error = String;

  fn try_from(raw: RawLayer) -> Result<Self, String> {
    match raw {
      RawLayer {
        run: Some(_),
        feature: Some(_),
        ..
      } => Err("a layer is one of `run` or `feature`, not both".into()),
      RawLayer {
        run: Some(_),
        with: Some(_),
        ..
      } => Err("`with` sets a `feature` layer's options".into()),
      RawLayer {
        feature: Some(reference),
        with,
        ..
      } => {
        if reference.trim().is_empty() {
          return Err("`feature` reference cannot be empty".into());
        }
        Ok(Self::Feature {
          reference,
          options: with.unwrap_or_default(),
        })
      }
      RawLayer { with: Some(_), .. } => Err("`with` sets a `feature` layer's options".into()),
      RawLayer {
        run: Some(script), ..
      } => Ok(Self::Run(script)),
      RawLayer { .. } => Err("a layer needs `run` or `feature`".into()),
    }
  }
}

/// Image files append their layers along `extends`.
const MERGE: merge::Rules = merge::Rules {
  prepend: &[],
  append: &[merge::Append {
    path: &["layers"],
    key: same_feature,
  }],
};

/// What makes two layers the same along `extends`: the same feature
/// reference without its tag, so a child layer that names a parent's feature
/// at another version replaces it in place. A `run` layer is never the same
/// as another.
pub(crate) fn same_feature(layer: &Value) -> Option<String> {
  layer.get("feature").and_then(Value::as_str).map(|reference| untagged(reference).to_string())
}

/// `reference` without its `:TAG` or `@DIGEST`.
// NOTICE: written by hand rather than with an OCI reference parser: the
// workspace has none, and a local feature directory (`./features/x`) is not
// an OCI reference. A tag is the text after the last `:` of the last path
// segment, so a registry's port (`localhost:5000/x`) stays.
pub fn untagged(reference: &str) -> &str {
  let reference = reference.split_once('@').map_or(reference, |(name, _)| name);
  let segment = reference.rfind('/').map_or(0, |slash| slash + 1);
  match reference[segment..].rfind(':') {
    Some(colon) => &reference[..segment + colon],
    None => reference,
  }
}

/// Reads an image file and its `extends` chain.
pub fn load(source: &Source) -> Result<Definition, Error> {
  let document = read(source)?;
  source::deserialize(document, Kind::Image, source)
}

/// Reads an image file and its `extends` chain into self-contained YAML.
pub fn resolve(source: &Source) -> Result<String, Error> {
  let document = read(source)?;
  source::serialize(&document, Kind::Image, source)
}

/// An image file and its `extends` chain as one merged document, checked to
/// be a definition.
fn read(source: &Source) -> Result<Value, Error> {
  let document = source::read_chain(source, Kind::Image, &MERGE)?;
  let _: Definition = source::deserialize(document.clone(), Kind::Image, source)?;
  Ok(document)
}

/// Whether `image`, as `actor.image` has it, is a path to an image file: it
/// starts with `./`, `../` or `/`, or ends in `.yaml`. The syntax decides,
/// not what exists, so a mistyped path is an error rather than a pull.
pub fn is_file(image: &str) -> bool {
  ["./", "../", "/"].iter().any(|prefix| image.starts_with(prefix)) || image.ends_with(".yaml")
}

/// Replaces an image file path in a merged profile's `actor.image` with the
/// file's merged definition, so the profile carries its image inline: a
/// daemon then reads no path on the client's machine. A relative path
/// resolves against `base`, a leading `~` against the reader's home.
/// Returns the resolved image file path when `actor.image` named one.
pub(crate) fn inline(profile: &mut Value, base: &Path) -> Result<Option<PathBuf>, Error> {
  let Some(image) = profile.get_mut("actor").and_then(|actor| actor.get_mut("image")) else {
    return Ok(None);
  };
  let Some(path) = image.as_str().filter(|path| is_file(path)) else {
    return Ok(None);
  };
  let path = shellexpand::tilde(path);
  let path = path.strip_prefix("./").unwrap_or(&path);
  let resolved = base.join(PathBuf::from(path));
  *image = read(&Source::File(resolved.clone()))?;
  Ok(Some(resolved))
}

// `actor.image` is a reference or a definition by its form: a string or a
// map.
impl<'de> Deserialize<'de> for Image {
  fn deserialize<D: de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
    deserializer.deserialize_any(ImageVisitor)
  }
}

struct ImageVisitor;

impl<'de> Visitor<'de> for ImageVisitor {
  type Value = Image;

  fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
    f.write_str("an image reference, a path to an image file or an image definition")
  }

  fn visit_str<E: de::Error>(self, image: &str) -> Result<Image, E> {
    if image.starts_with("builtin:") {
      return Err(E::custom(format!(
        "needy ships no built-in images, so {image} names none: `builtin:` names recipes and profiles; \
         write an image definition and install the agent in one of its layers"
      )));
    }
    if is_file(image) {
      // NOTICE: `profile::load` and `profile::resolve` replace an image
      // file's path with its definition (`inline`) before typing, so this is
      // reached only by a document typed some other way.
      return Err(E::custom(format!("{image} is an image file, which only a profile's loader reads")));
    }
    Ok(Image::Reference(image.to_string()))
  }

  fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Image, A::Error> {
    Definition::deserialize(de::value::MapAccessDeserializer::new(map)).map(Image::Definition)
  }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
