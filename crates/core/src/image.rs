//! The build plan of an image definition: what to build over a root, in
//! which order, under which cache keys, and the environment the image's
//! programs get. No IO: the root is resolved to a digest elsewhere, and
//! building, storing and booting the steps belong to the daemon and the
//! layer store. See "Resolving and planning a build" in
//! `docs/ai/references/architecture/2026-11-17-images-and-run-data-spec.md`.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use needy_config::image::{Definition, Layer};
use needy_config::profile::User;
use serde_json::json;
use sha2::{Digest as _, Sha256};

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
// TODO(images): the `feature` input (a feature's digest and its options)
// comes with slice 3 of the images and run data spec, with the feature
// layers `needy-config` refuses until then.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
  /// A shell script run by `sh -c` as root.
  Run(String),
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

/// Plans `definition` over the root `root` for `user`, with the image's
/// commit layer, if it has one, on top.
///
/// A step's key is the digest of its parent's key and its input, so a
/// changed layer changes its key and every key above it, and appending a
/// layer keeps the keys below. The image's `env` is not a build input: the
/// steps run without it and it changes no key.
// NOTICE: a step's parent is its parent's key, not the digest of the layer
// built under it. Builds are not reproducible (timestamps, downloads), so a
// key over built content would be known only after the parent was built,
// and the plan could not name a whole image's keys before building.
// TODO(images): with `containerEnv` (slice 3), a feature's environment is an
// input of the steps above it, and enters their keys.
pub fn plan(definition: &Definition, root: &Digest, user: &User, commit: Option<&Commit>) -> Plan {
  let base = key(&json!({
    "root": root.to_string(),
    "user": { "name": user.name, "uid": user.uid, "gid": user.gid },
  }));
  let mut parent = base;
  let steps = definition
    .layers
    .iter()
    .map(|layer| {
      let (input, resolved) = match layer {
        Layer::Run(script) => (Input::Run(script.clone()), json!({ "run": script })),
      };
      parent = key(&json!({ "parent": parent.to_string(), "input": resolved }));
      Step { input, key: parent }
    })
    .collect();
  let (path, env) = environment(&definition.env);
  Plan {
    root: *root,
    user: user.clone(),
    base,
    steps,
    commit: commit.cloned(),
    env,
    path,
  }
}

/// The key of a step's resolved input. JSON keeps the fields apart, so no
/// two inputs share their bytes.
fn key(input: &serde_json::Value) -> Digest {
  Digest::of(input.to_string().as_bytes())
}

/// The image's `PATH` and the rest of its environment. `$PATH` and
/// `${PATH}` in a value expand to [`SYSTEM_PATH`], so `PATH:
/// /opt/tool/bin:${PATH}` adds a directory before it; other variables are
/// left as written, for the program that reads them.
// TODO(images): with `containerEnv` (slice 3), the environment is built in
// layer order and a value expands against the environment so far, not only
// against `PATH`.
fn environment(env: &BTreeMap<String, String>) -> (String, BTreeMap<String, String>) {
  let expand = |value: &str| shellexpand::env_with_context_no_errors(value, |name| (name == "PATH").then_some(SYSTEM_PATH)).into_owned();
  let mut env: BTreeMap<_, _> = env.iter().map(|(name, value)| (name.clone(), expand(value))).collect();
  let path = env.remove("PATH").unwrap_or_else(|| SYSTEM_PATH.into());
  (path, env)
}

#[cfg(test)]
#[path = "image_test.rs"]
mod tests;
