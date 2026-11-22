//! The build plan of an image definition: what to build over a root, in
//! which order, under which cache keys, and the environment the image's
//! programs get. No IO: the root and features are resolved elsewhere, and
//! building, storing and booting the steps belong to the daemon and the
//! layer store. See "Resolving and planning a build" in
//! `docs/ai/references/architecture/2026-11-17-images-and-run-data-spec.md`.

pub mod feature;

use std::collections::{BTreeMap, HashSet};

pub use devcontainer_features::{Digest, InvalidDigest};
use devcontainer_features::{FeatureRequest, GraphError, OptionValue};
use needy_config::image::{Definition, Hooks, Layer, untagged};
use needy_config::profile::{BUILT_CONTAINER_IMAGE, CONTAINER_IMAGE, OnConflict, User};
use serde_json::json;

pub use self::feature::{
  FeatureCustomizations, FeatureError, FeatureFileSpec, FeatureMetadata, LifecycleCommand, LifecycleEntry, NeedyCustomizations, OptionSpec,
  OptionValue as FeatureOptionValue, sanitize_option_name,
};
pub use needy_config::image::DEFAULT_ROOT;

/// Reference name for needy's built-in `sshfs` Dev Container feature (decision
/// E8 in `docs/ai/references/architecture/2026-11-17-images-and-run-data-spec.md`).
pub const BUILTIN_SSHFS: &str = "builtin:sshfs";

/// Whether `root` names needy's own base actor image (which already includes
/// `sshfs` and the `/.needy` layer).
pub fn is_needy_base_image(root: &str) -> bool {
  let name = untagged(root);
  root == CONTAINER_IMAGE || root == BUILT_CONTAINER_IMAGE || name == "needy-actor" || name == "ghcr.io/needy-dev/needy-actor"
}

/// Whether `definition` names a custom OCI root (different from needy's own
/// base image) and does not already include an `sshfs` feature layer, so
/// [`BUILTIN_SSHFS`] must be prepended as its first layer (decision E8).
pub fn needs_sshfs(definition: &Definition) -> bool {
  let Some(root) = definition.root.as_deref() else {
    return false;
  };
  if is_needy_base_image(root) {
    return false;
  }
  !definition.layers.iter().any(|layer| match layer {
    Layer::Feature { reference, .. } => {
      let id = untagged(reference);
      id == BUILTIN_SSHFS || id.ends_with("/sshfs")
    }
    Layer::Run(_) => false,
  })
}

/// The `PATH` an actor's own programs are found on before an image adds to
/// it; the image's `PATH` starts from it.
// Source: https://github.com/moby/moby/blob/v28.5.1/oci/defaults.go#L19
// (`defaultUnixPathEnv`, the PATH a container gets) and Debian's root PATH,
// https://sources.debian.org/src/base-files/13.8%2Bdeb13u7/share/profile/#L5
// Reference: https://github.com/systemd/systemd/blob/v257/src/basic/path-util.h#L23
// (`DEFAULT_PATH_COMPAT`, the same string).
pub const SYSTEM_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// What building an image definition takes, from the root up: the root and
/// the profile's user under it, one step per layer, the commit layer on top,
/// and the environment of the image's programs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
  /// The resolved OCI root image reference (`definition.root` when set, else
  /// [`DEFAULT_ROOT`]).
  pub root_image: String,
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
  /// Lifecycle hooks for runs of this image: features' hooks first in resolved
  /// layer order, followed by the image definition's hooks.
  pub hooks: Hooks,
  /// Sync mappings declared by features (`customizations.needy.files`),
  /// collected in resolved layer order with `~` expanded for [`Plan::user`].
  pub sync_files: Vec<FeatureSyncFile>,
}

/// A sync mapping declared by a Dev Container feature's
/// `customizations.needy.files`, resolved for a specific [`Plan::user`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeatureSyncFile {
  /// Mapping name (key in `customizations.needy.files`).
  pub name: String,
  /// Store key under needy's `<data_dir>/files/<store>` directory.
  /// Defaults to [`Self::name`] when omitted in `devcontainer-feature.json`.
  pub store: String,
  /// Target path inside the actor, with leading `~` expanded to the profile
  /// user's home directory.
  pub path: String,
  /// rclone-style ordered `+` and `-` rules; first match wins.
  pub filter: Vec<String>,
  /// Conflict resolution choices for entries changed by both the run and the
  /// store, in the order written.
  pub on_conflict: Vec<(String, OnConflict)>,
  /// The feature reference that declared this mapping.
  pub feature: String,
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
  #[error("feature ordering cycle detected across `dependsOn`/`installsAfter`: {cycle}")]
  OrderingCycle { cycle: String },
  #[error(
    "features `{first_feature}` (mapping `{first_name}`) and `{second_feature}` (mapping `{second_name}`) both claim actor path `{path}`"
  )]
  SyncFilePathConflict {
    path: String,
    first_feature: String,
    first_name: String,
    second_feature: String,
    second_name: String,
  },
  #[error("features `{first_feature}` and `{second_feature}` both declare sync mapping `{name}`")]
  SyncFileNameConflict {
    name: String,
    first_feature: String,
    second_feature: String,
  },
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
    options: BTreeMap<String, OptionValue>,
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
  let effective_layers: Vec<Layer>;
  let layers = if needs_sshfs(definition) {
    effective_layers = std::iter::once(Layer::Feature {
      reference: BUILTIN_SSHFS.to_string(),
      options: BTreeMap::new(),
    })
    .chain(definition.layers.iter().cloned())
    .collect();
    &effective_layers[..]
  } else {
    &definition.layers[..]
  };
  let resolved_layers = resolve_layers(layers, features)?;

  let base = key(&json!({
    "root": root.to_string(),
    "user": { "name": user.name, "uid": user.uid, "gid": user.gid },
  }));
  let mut parent = base;
  let mut steps = Vec::with_capacity(resolved_layers.len());
  let mut acc_env = BTreeMap::from([("PATH".to_string(), SYSTEM_PATH.to_string())]);
  let mut hooks = Hooks::default();
  let mut sync_files: Vec<FeatureSyncFile> = Vec::new();

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
        hooks.start.extend(resolved.metadata.start_hooks());
        collect_feature_sync_files(&reference, &resolved.metadata, user, &mut sync_files)?;
        let env = resolved.metadata.inner.resolve_options(&options);
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
  hooks.start.extend(definition.hooks.start.iter().cloned());
  hooks.stop.extend(definition.hooks.stop.iter().cloned());

  Ok(Plan {
    root_image: definition.effective_root().to_string(),
    root: *root,
    user: user.clone(),
    base,
    steps,
    commit: commit.cloned(),
    env: acc_env,
    path,
    hooks,
    sync_files,
  })
}

/// Collects `customizations.needy.files` entries from one feature into `out`,
/// expanding leading `~` for `user` and rejecting duplicate mapping names or
/// conflicting actor paths across features.
fn collect_feature_sync_files(
  reference: &str,
  metadata: &FeatureMetadata,
  user: &User,
  out: &mut Vec<FeatureSyncFile>,
) -> Result<(), PlanError> {
  let Some(needy) = &metadata.customizations.needy else {
    return Ok(());
  };
  for (name, spec) in &needy.files {
    let expanded = user.expand_home(spec.path.trim());
    let norm = trim_trailing_slashes(&expanded);
    for existing in out.iter() {
      if existing.name == *name {
        return Err(PlanError::SyncFileNameConflict {
          name: name.clone(),
          first_feature: existing.feature.clone(),
          second_feature: reference.to_string(),
        });
      }
      if trim_trailing_slashes(&existing.path) == norm {
        return Err(PlanError::SyncFilePathConflict {
          path: norm.to_string(),
          first_feature: existing.feature.clone(),
          first_name: existing.name.clone(),
          second_feature: reference.to_string(),
          second_name: name.clone(),
        });
      }
    }
    out.push(FeatureSyncFile {
      name: name.clone(),
      store: spec.store.clone().unwrap_or_else(|| name.clone()),
      path: expanded,
      filter: spec.filter.clone(),
      on_conflict: spec.on_conflict.clone(),
      feature: reference.to_string(),
    });
  }
  Ok(())
}

/// `path` without trailing slashes, except the root.
fn trim_trailing_slashes(path: &str) -> &str {
  match path.trim_end_matches('/') {
    "" => path,
    trimmed => trimmed,
  }
}

/// Expands `layers` by delegating each contiguous feature block to
/// `devcontainer_features::resolve_order_excluding` (handling transitive
/// `dependsOn` expansion, cycle detection, and `installsAfter` topological
/// sorting) while preserving `Run` layer barriers and enforcing `needy`'s
/// `check_allowed` security policy on every resolved feature.
fn resolve_layers<'a>(layers: &[Layer], features: &'a BTreeMap<String, ResolvedFeature>) -> Result<Vec<ResolvedLayer<'a>>, PlanError> {
  let raw_metadata: BTreeMap<String, devcontainer_features::FeatureMetadata> =
    features.iter().map(|(k, v)| (k.clone(), v.metadata.inner.clone())).collect();

  let mut out = Vec::with_capacity(layers.len());
  let mut emitted: HashSet<String> = HashSet::new();
  let mut pending: Vec<FeatureRequest> = Vec::new();

  let flush_block =
    |pending: &mut Vec<FeatureRequest>, emitted: &mut HashSet<String>, out: &mut Vec<ResolvedLayer<'a>>| -> Result<(), PlanError> {
      if pending.is_empty() {
        return Ok(());
      }
      let ordered = devcontainer_features::resolve_order_excluding(pending, &raw_metadata, emitted).map_err(|err| match err {
        GraphError::MissingFeature { reference } => PlanError::MissingFeature { reference },
        GraphError::DependencyCycle { cycle } => PlanError::DependencyCycle { cycle },
        GraphError::OrderingCycle { cycle } => PlanError::OrderingCycle { cycle },
      })?;
      for feat in ordered {
        let resolved = features.get(&feat.reference).ok_or_else(|| PlanError::MissingFeature {
          reference: feat.reference.clone(),
        })?;
        resolved.metadata.check_allowed(&feat.reference)?;
        emitted.insert(untagged(&feat.reference).to_string());
        out.push(ResolvedLayer::Feature {
          reference: feat.reference,
          resolved,
          options: feat.options,
        });
      }
      pending.clear();
      Ok(())
    };

  for layer in layers {
    match layer {
      Layer::Run(script) => {
        flush_block(&mut pending, &mut emitted, &mut out)?;
        out.push(ResolvedLayer::Run(script.clone()));
      }
      Layer::Feature { reference, options } => {
        pending.push(FeatureRequest {
          reference: reference.clone(),
          options: options.iter().map(|(k, v)| (k.clone(), self::feature::to_option_value(v))).collect(),
        });
      }
    }
  }
  flush_block(&mut pending, &mut emitted, &mut out)?;
  Ok(out)
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
