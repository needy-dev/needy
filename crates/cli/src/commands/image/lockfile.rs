//! Image lockfiles, target resolution, and `run:` snippet formatting.
//!
//! The CLI owns the user's files and writes an image's commit pointer to a
//! sibling `<file>.lock` file rather than rewriting the user's image or
//! profile YAML. See `docs/ai/references/architecture/2026-11-17-images-and-run-data-spec.md`
//! ("Building and editing", "A commit's `run:` snippet").

use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use needy_api_client::proto::v1::{ImageCommit, ImageUser, RecordedCommand};
use needy_config::Source;
use needy_config::image::Image;
use needy_config::profile::{ActorDriver, User};
use serde::{Deserialize, Serialize};

use crate::cli::Context;

/// Header written to managed lockfiles.
const LOCKFILE_HEADER: &str = "# Managed by needy. Do not edit by hand.\n";

/// What an image lockfile records.
// TODO(images): feature lock entries (resolved versions and digests) come with
// slice 3 of the images and run data spec.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lockfile {
  /// The image's current commit layer and the base cache key it was made on.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub commit: Option<LockCommit>,
}

/// One commit entry in a lockfile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockCommit {
  pub layer: String,
  pub base: String,
}

impl LockCommit {
  pub fn to_proto(&self) -> Result<ImageCommit> {
    needy_core::image::Digest::parse(&self.layer).with_context(|| format!("invalid commit layer digest in lockfile: {}", self.layer))?;
    needy_core::image::Digest::parse(&self.base).with_context(|| format!("invalid commit base digest in lockfile: {}", self.base))?;
    Ok(ImageCommit {
      layer: self.layer.clone(),
      base: self.base.clone(),
    })
  }

  pub fn from_proto(commit: &ImageCommit) -> Result<Self> {
    needy_core::image::Digest::parse(&commit.layer).with_context(|| format!("invalid commit layer digest from daemon: {}", commit.layer))?;
    needy_core::image::Digest::parse(&commit.base).with_context(|| format!("invalid commit base digest from daemon: {}", commit.base))?;
    Ok(Self {
      layer: commit.layer.clone(),
      base: commit.base.clone(),
    })
  }
}

/// The lockfile path next to an image file (or a profile carrying an inline
/// image definition): `<path>.lock`.
pub(crate) fn path_for(source: &Path) -> PathBuf {
  let mut name = source.as_os_str().to_os_string();
  name.push(".lock");
  PathBuf::from(name)
}

/// Reads a lockfile, returning an empty [`Lockfile`] when the file does not
/// exist.
pub(crate) fn read(path: &Path) -> Result<Lockfile> {
  let text = match std::fs::read_to_string(path) {
    Ok(text) => text,
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Lockfile::default()),
    Err(e) => return Err(e).with_context(|| format!("cannot read lockfile {}", path.display())),
  };
  serde_yaml::from_str(&text).with_context(|| format!("cannot parse lockfile {}", path.display()))
}

/// Atomically writes `lock` to `path`. Removes `path` when `lock` has no
/// entries (for example after rolling back past the first commit).
pub(crate) fn write(path: &Path, lock: &Lockfile) -> Result<()> {
  if lock.commit.is_none() {
    match std::fs::remove_file(path) {
      Ok(()) => return Ok(()),
      Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
      Err(e) => return Err(e).with_context(|| format!("cannot remove empty lockfile {}", path.display())),
    }
  }
  let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
  std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
  let body = serde_yaml::to_string(lock).context("cannot serialize lockfile")?;
  let staged = dir.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
  let mut file =
    std::fs::OpenOptions::new().write(true).create_new(true).open(&staged).with_context(|| format!("cannot stage {}", path.display()))?;
  file.write_all(LOCKFILE_HEADER.as_bytes()).with_context(|| format!("cannot write {}", path.display()))?;
  file.write_all(body.as_bytes()).with_context(|| format!("cannot write {}", path.display()))?;
  file.sync_all().with_context(|| format!("cannot sync {}", path.display()))?;
  drop(file);
  std::fs::rename(&staged, path).with_context(|| format!("cannot persist lockfile {}", path.display()))?;
  Ok(())
}

/// A resolved image definition target for `image build`, `image edit`, or
/// `image rollback`.
pub(crate) struct ImageTarget {
  /// Stable identifier for the image (canonical path of the image file, or of
  /// the profile for an inline definition).
  pub image_name: String,
  /// Path to the sibling lockfile owned by the CLI.
  pub lockfile: PathBuf,
  /// Self-contained image definition YAML (its `extends` chain merged).
  pub image_yaml: String,
  /// Actor driver name, or empty to let the daemon choose its host VM driver.
  pub driver: String,
  /// Profile user for build plan cache keys, if known.
  pub user: Option<ImageUser>,
}

/// Resolves either an image file path (`IMAGE`) or a profile's `actor.image`
/// (`--profile P`) into an [`ImageTarget`].
pub(crate) fn resolve_target(
  image: Option<&Path>,
  profile: Option<&Path>,
  driver_override: Option<ActorDriver>,
  ctx: &Context,
) -> Result<ImageTarget> {
  match (image, profile) {
    (Some(_), Some(_)) => bail!("name an image file or `--profile`, not both"),
    (None, None) => bail!("name an image file or pass `--profile PROFILE`"),
    (Some(image_path), None) => {
      let canonical = std::fs::canonicalize(image_path).with_context(|| format!("cannot read image file {}", image_path.display()))?;
      let image_yaml = needy_config::image::resolve(&Source::file(&canonical))?;
      let lockfile = path_for(&canonical);
      Ok(ImageTarget {
        image_name: canonical.display().to_string(),
        lockfile,
        image_yaml,
        driver: driver_override.map(|d| d.as_str().to_string()).unwrap_or_default(),
        user: None,
      })
    }
    (None, Some(profile_arg)) => {
      let profile_path = ctx.profile(profile_arg)?;
      let canonical_profile =
        std::fs::canonicalize(&profile_path).with_context(|| format!("cannot read profile {}", profile_path.display()))?;
      let resolved = needy_config::profile::resolve(&Source::file(&canonical_profile))?;
      let loaded = needy_config::profile::load(&Source::text(resolved.yaml.clone(), PathBuf::new()))?;
      let Some(Image::Definition(_)) = &loaded.actor.image else {
        bail!(
          "profile {} does not use an image definition in `actor.image` (it uses an OCI image reference or the base image)",
          profile_arg.display()
        );
      };
      // Extract the already-resolved `actor.image` mapping from the resolved profile YAML.
      let doc: serde_yaml::Value = serde_yaml::from_str(&resolved.yaml)?;
      let image_value = doc.get("actor").and_then(|a| a.get("image")).context("resolved profile is missing `actor.image`")?;
      let image_yaml = serde_yaml::to_string(image_value)?;
      let source_path = match &resolved.image_file {
        Some(file) => std::fs::canonicalize(file).unwrap_or_else(|_| file.clone()),
        None => canonical_profile,
      };
      let driver = driver_override.unwrap_or(loaded.actor.driver).as_str().to_string();
      Ok(ImageTarget {
        image_name: source_path.display().to_string(),
        lockfile: path_for(&source_path),
        image_yaml,
        driver,
        user: Some(proto_user(&loaded.user)),
      })
    }
  }
}

fn proto_user(user: &User) -> ImageUser {
  ImageUser {
    name: user.name.clone(),
    uid: user.uid,
    gid: user.gid,
  }
}

/// Formats the commands recorded during an edit session as a pasteable YAML
/// `run:` layer snippet. Returns `None` when no commands were recorded.
pub(crate) fn format_run_snippet(commands: &[RecordedCommand]) -> Option<String> {
  if commands.is_empty() {
    return None;
  }
  let mut lines = Vec::new();
  let mut current_cwd = "/";
  for cmd in commands {
    if cmd.argv.is_empty() {
      continue;
    }
    if !cmd.cwd.is_empty() && cmd.cwd != current_cwd {
      lines.push(format!("cd {}", shell_words::quote(&cmd.cwd)));
      current_cwd = &cmd.cwd;
    }
    lines.push(shell_words::join(&cmd.argv));
  }
  if lines.is_empty() {
    return None;
  }
  let mut out = String::from("- run: |\n");
  for line in lines {
    out.push_str("    ");
    out.push_str(&line);
    out.push('\n');
  }
  Some(out)
}

#[cfg(test)]
#[path = "lockfile_test.rs"]
mod tests;
