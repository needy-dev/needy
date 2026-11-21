//! The build plan of an image definition: what to build over a root, in
//! which order, under which cache keys, and the environment the image's
//! programs get. No IO: the root and features are resolved elsewhere, and
//! building, storing and booting the steps belong to the daemon and the
//! layer store. See "Resolving and planning a build" in
//! `docs/ai/references/architecture/2026-11-17-images-and-run-data-spec.md`.

pub mod feature;

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::str::FromStr;

use needy_config::image::{Definition, FeatureOption, Layer, untagged};
use needy_config::profile::User;
use serde_json::json;
use sha2::{Digest as _, Sha256};

pub use self::feature::{FeatureError, FeatureMetadata, OptionSpec, sanitize_option_name};

/// The `PATH` an actor's own programs are found on before an image adds to
/// it; the image's `PATH` starts from it.
// Source: https://github.com/moby/moby/blob/v28.5.1/oci/defaults.go#L19
// (`defaultUnixPathEnv`, the PATH a container gets) and Debian's root PATH,
// https://sources.debian.org/src/base-files/13.8%2Bdeb13u7/share/profile/#L5
// Reference: https://github.com/systemd/systemd/blob/v257/src/basic/path-util.h#L23
// (`DEFAULT_PATH_COMPAT`, the same string).
pub const SYSTEM_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// A SHA-256 content digest written as OCI writes one, `sha256:<hex>`: a
/// root's, a layer's, a snapshot's, or a cache key.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Digest([u8; 32]);

/// Text that is not a `sha256:` digest of 64 lowercase hex digits.
#[derive(Debug, thiserror::Error)]
#[error("{0} is not a digest: expected sha256: and 64 lowercase hex digits")]
pub struct InvalidDigest(String);

impl Digest {
  /// Reads `sha256:<64 lowercase hex digits>`.
  // NOTICE: only SHA-256: needy computes every digest it keys on, and OCI
  // registries name image content by it. Checked by hand because the
  // workspace has no OCI digest parser and the form is one prefix and a
  // length. Reference: https://github.com/opencontainers/image-spec/blob/v1.1.1/descriptor.md#digests
  pub fn parse(text: &str) -> Result<Self, InvalidDigest> {
    let hex = text.strip_prefix("sha256:").ok_or_else(|| InvalidDigest(text.into()))?;
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
      return Err(InvalidDigest(text.into()));
    }
    let mut bytes = [0u8; 32];
    let (pairs, _) = hex.as_bytes().as_chunks::<2>();
    for (byte, &[hi, lo]) in bytes.iter_mut().zip(pairs) {
      *byte = (hex_nibble(hi) << 4) | hex_nibble(lo);
    }
    Ok(Self(bytes))
  }

  /// The digest of `content`.
  pub fn of(content: &[u8]) -> Self {
    Self(Sha256::digest(content).into())
  }

  /// Constructs a digest directly from its 32 raw SHA-256 bytes.
  pub fn from_bytes(bytes: [u8; 32]) -> Self {
    Self(bytes)
  }

  /// The 32 raw SHA-256 bytes.
  pub fn as_bytes(&self) -> &[u8; 32] {
    &self.0
  }

  /// The 64 lowercase hex digits, without the `sha256:` prefix.
  pub fn hex(&self) -> String {
    use std::fmt::Write as _;
    self.0.iter().fold(String::with_capacity(64), |mut hex, byte| {
      // NOTICE: writing to a `String` cannot fail.
      let _ = write!(hex, "{byte:02x}");
      hex
    })
  }
}

fn hex_nibble(byte: u8) -> u8 {
  match byte {
    b'0'..=b'9' => byte - b'0',
    b'a'..=b'f' => byte - b'a' + 10,
    _ => unreachable!("validated lowercase hex above"),
  }
}

impl fmt::Display for Digest {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "sha256:{}", self.hex())
  }
}

impl fmt::Debug for Digest {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    fmt::Display::fmt(self, f)
  }
}

impl FromStr for Digest {
  type Err = InvalidDigest;

  fn from_str(text: &str) -> Result<Self, Self::Err> {
    Self::parse(text)
  }
}

impl serde::Serialize for Digest {
  fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_str(self)
  }
}

impl<'de> serde::Deserialize<'de> for Digest {
  fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
    let text = String::deserialize(deserializer)?;
    Self::parse(&text).map_err(serde::de::Error::custom)
  }
}

/// What building an image definition takes, from the root up: the root and
/// the profile's user under it, one step per layer, the commit layer on top,
/// and the environment of the image's programs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
  /// The root's digest, as resolution gave it.
  pub root: Digest,
  /// The profile's user, created in the root before the first step (A3 in
  /// the environments and snapshots design), so steps can install for it.
  pub user: User,
  /// The key of the root with the user in it, the parent of the first
  /// step. Every key above includes it, so a different root or user (name,
  /// UID or GID) is a different image.
  pub base: Digest,
  /// The built layers, in order: each one runs over the one below.
  pub steps: Vec<Step>,
  /// The image's hand edits, kept as one layer over the steps.
  pub commit: Option<Commit>,
  /// The image's environment, without `PATH`.
  pub env: BTreeMap<String, String>,
  /// The image's `PATH`. It is kept out of `env` because it goes between
  /// the trampolines and the route stubs of the driving program's `PATH`
  /// rather than replacing it whole.
  pub path: String,
}

/// One layer to build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
  /// What the step runs.
  pub input: Input,
  /// The parent's key and this step's input: two steps with one key build
  /// the same layer, so a layer built under a key is reused.
  pub key: Digest,
}

/// A step's resolved input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
  /// A shell script run by `sh -c` as root.
  Run(String),
  /// A Dev Container feature installed by its `install.sh` as root.
  Feature {
    /// The feature reference as written (e.g. `ghcr.io/devcontainers/features/node:1`).
    reference: String,
    /// The resolved content digest of the feature artifact.
    digest: Digest,
    /// Sanitized option environment variables (with defaults filled in),
    /// written to `devcontainer-features.env`.
    env: BTreeMap<String, String>,
  },
}

/// A resolved Dev Container feature ready for build planning: its content
/// digest and parsed `devcontainer-feature.json` metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedFeature {
  /// The content digest of the resolved feature artifact.
  pub digest: Digest,
  /// Parsed `devcontainer-feature.json` metadata.
  pub metadata: FeatureMetadata,
}

/// Errors that can prevent an image definition from being planned.
#[derive(Debug, thiserror::Error)]
pub enum PlanError {
  #[error("feature `{reference}` was not resolved before planning")]
  MissingFeature { reference: String },
  #[error(transparent)]
  RefusedFeature(#[from] FeatureError),
  #[error("feature `dependsOn` cycle detected: {cycle}")]
  DependencyCycle { cycle: String },
  #[error("feature `{feature}` declares `installsAfter` `{after}`, which appears after it in the layer order")]
  InstallsAfterViolation { feature: String, after: String },
}

/// An image's commit layer as its lockfile records it: the layer that holds
/// its hand edits, and the key of the top it was made on.
// TODO(images): `needy image edit` writes this to the image's lockfile, and
// `run create` reads it back, in step 8 of the slice 1 plan
// (`2026-11-17-images-slice-1-plan.md`); nothing makes one yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
  /// The commit layer's digest.
  pub layer: Digest,
  /// [`Plan::top`] when the commit was made.
  pub base: Digest,
}

impl Plan {
  /// The key of the last step, or the base when there is none: what a
  /// commit is made on.
  pub fn top(&self) -> &Digest {
    self.steps.last().map_or(&self.base, |step| &step.key)
  }

  /// Whether the commit layer was made on another top than this plan's (a
  /// layer below, the root or the user changed). It is applied on the new
  /// top all the same, and the caller reports that it was (owner decision
  /// of 2026-11-17 in the images and run data spec).
  pub fn commit_rebased(&self) -> bool {
    self.commit.as_ref().is_some_and(|commit| commit.base != *self.top())
  }
}

enum ResolvedLayer<'a> {
  Run(String),
  Feature {
    reference: String,
    resolved: &'a ResolvedFeature,
    options: BTreeMap<String, FeatureOption>,
  },
}

/// Plans `definition` over the root `root` for `user`, with the resolved
/// Dev Container `features` and the image's commit layer, if it has one, on
/// top.
///
/// A step's key is the digest of its parent's key and its input, so a
/// changed layer changes its key and every key above it, and appending a
/// layer keeps the keys below. The image's `env` is not a build input: the
/// steps run without it and it changes no key.
// NOTICE: a step's parent is its parent's key, not the digest of the layer
// built under it. Builds are not reproducible (timestamps, downloads), so a
// key over built content would be known only after the parent was built,
// and the plan could not name a whole image's keys before building.
// NOTICE: a feature's `containerEnv` comes from `devcontainer-feature.json`
// inside the feature artifact, so changing it changes the feature's digest
// and therefore its key and every step's key above it.
pub fn plan(
  definition: &Definition,
  root: &Digest,
  user: &User,
  commit: Option<&Commit>,
  features: &BTreeMap<String, ResolvedFeature>,
) -> Result<Plan, PlanError> {
  let resolved_layers = resolve_layers(&definition.layers, features)?;
  check_installs_after(&resolved_layers)?;

  let base = key(&json!({
    "root": root.to_string(),
    "user": { "name": user.name, "uid": user.uid, "gid": user.gid },
  }));
  let mut parent = base;
  let mut steps = Vec::with_capacity(resolved_layers.len());
  let mut acc_env = BTreeMap::from([("PATH".to_string(), SYSTEM_PATH.to_string())]);

  for layer in resolved_layers {
    let (input, resolved_input) = match layer {
      ResolvedLayer::Run(script) => {
        let resolved_input = json!({ "run": script });
        (Input::Run(script), resolved_input)
      }
      ResolvedLayer::Feature {
        reference,
        resolved,
        options,
      } => {
        apply_env(&mut acc_env, &resolved.metadata.container_env);
        let env = resolved.metadata.resolve_options(&options);
        let resolved_input = json!({
          "feature": {
            "digest": resolved.digest.to_string(),
            "env": env,
          }
        });
        (
          Input::Feature {
            reference,
            digest: resolved.digest,
            env,
          },
          resolved_input,
        )
      }
    };
    parent = key(&json!({ "parent": parent.to_string(), "input": resolved_input }));
    steps.push(Step { input, key: parent });
  }

  apply_env(&mut acc_env, &definition.env);
  let path = acc_env.remove("PATH").unwrap_or_else(|| SYSTEM_PATH.into());

  Ok(Plan {
    root: *root,
    user: user.clone(),
    base,
    steps,
    commit: commit.cloned(),
    env: acc_env,
    path,
  })
}

/// Expands `layers` by inserting `dependsOn` features before their dependents
/// recursively, rejecting missing features, refused properties, and cycles.
fn resolve_layers<'a>(layers: &[Layer], features: &'a BTreeMap<String, ResolvedFeature>) -> Result<Vec<ResolvedLayer<'a>>, PlanError> {
  let mut out = Vec::with_capacity(layers.len());
  let mut visiting = Vec::new();
  let mut emitted = HashSet::new();

  for layer in layers {
    match layer {
      Layer::Run(script) => out.push(ResolvedLayer::Run(script.clone())),
      Layer::Feature { reference, options } => {
        resolve_feature(reference, options, features, &mut visiting, &mut emitted, &mut out)?;
      }
    }
  }
  Ok(out)
}

fn resolve_feature<'a>(
  reference: &str,
  options: &BTreeMap<String, FeatureOption>,
  features: &'a BTreeMap<String, ResolvedFeature>,
  visiting: &mut Vec<String>,
  emitted: &mut HashSet<String>,
  out: &mut Vec<ResolvedLayer<'a>>,
) -> Result<(), PlanError> {
  let id_key = untagged(reference).to_string();
  if emitted.contains(&id_key) {
    return Ok(());
  }
  if let Some(pos) = visiting.iter().position(|entry| entry == &id_key) {
    let mut cycle: Vec<&str> = visiting[pos..].iter().map(String::as_str).collect();
    cycle.push(&id_key);
    return Err(PlanError::DependencyCycle {
      cycle: cycle.join(" -> "),
    });
  }

  let resolved = features.get(reference).ok_or_else(|| PlanError::MissingFeature {
    reference: reference.to_string(),
  })?;
  resolved.metadata.check_allowed(reference)?;

  visiting.push(id_key.clone());
  for (dep_ref, dep_options) in &resolved.metadata.depends_on {
    resolve_feature(dep_ref, dep_options, features, visiting, emitted, out)?;
  }
  visiting.pop();

  if emitted.insert(id_key) {
    out.push(ResolvedLayer::Feature {
      reference: reference.to_string(),
      resolved,
      options: options.clone(),
    });
  }
  Ok(())
}

/// Checks every feature's `installsAfter` entries against the resolved layer
/// order; a contradiction between two present features is an error naming both.
fn check_installs_after(layers: &[ResolvedLayer<'_>]) -> Result<(), PlanError> {
  let positions: Vec<(usize, &str, &str)> = layers
    .iter()
    .enumerate()
    .filter_map(|(idx, layer)| match layer {
      ResolvedLayer::Feature {
        reference,
        resolved,
        ..
      } => Some((idx, untagged(reference), resolved.metadata.id.as_str())),
      ResolvedLayer::Run(_) => None,
    })
    .collect();

  for (idx, layer) in layers.iter().enumerate() {
    let ResolvedLayer::Feature {
      reference,
      resolved,
      ..
    } = layer
    else {
      continue;
    };
    for after in &resolved.metadata.installs_after {
      let after_untagged = untagged(after);
      if let Some((other_idx, _, _)) =
        positions.iter().find(|(_, pos_untagged, pos_id)| *pos_untagged == after_untagged || *pos_id == after.as_str())
        && *other_idx > idx
      {
        return Err(PlanError::InstallsAfterViolation {
          feature: reference.clone(),
          after: after.clone(),
        });
      }
    }
  }
  Ok(())
}

/// The key of a step's resolved input. JSON keeps the fields apart, so no
/// two inputs share their bytes.
fn key(input: &serde_json::Value) -> Digest {
  Digest::of(input.to_string().as_bytes())
}

/// Applies `entries` onto `acc`, expanding `$VAR` and `${VAR}` against the
/// environment accumulated so far and leaving unknown variables as written.
/// Non-`PATH` entries are applied before `PATH` so `PATH` can reference
/// variables declared alongside it in the same `containerEnv` or `env` map.
fn apply_env(acc: &mut BTreeMap<String, String>, entries: &BTreeMap<String, String>) {
  for (name, value) in entries {
    if name == "PATH" {
      continue;
    }
    let expanded = shellexpand::env_with_context_no_errors(value, |var| acc.get(var).map(String::as_str)).into_owned();
    acc.insert(name.clone(), expanded);
  }
  if let Some(value) = entries.get("PATH") {
    let expanded = shellexpand::env_with_context_no_errors(value, |var| acc.get(var).map(String::as_str)).into_owned();
    acc.insert("PATH".to_string(), expanded);
  }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
