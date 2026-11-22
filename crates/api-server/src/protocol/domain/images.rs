//! Proto <-> domain mappings for `ImageService`.

use std::collections::{BTreeMap, HashMap};

use needy_api_proto::v1;

use crate::control::{
  BuildStepOutcome, CommitHistoryEntry, ImageBuild, ImageBuildResult, ImageEditFinished, ImageEditStart, ImageEditStarted, ImageExport,
  ImageLog, ImageLogResult, ImageRollback, ImageRollbackResult, RecordedCommand,
};

use super::{Error, proto_exit};

pub fn image_commit(commit: v1::ImageCommit) -> Result<needy_core::image::Commit, Error> {
  let layer = needy_core::image::Digest::parse(&commit.layer).map_err(|e| Error::Invalid(format!("image commit layer: {e}")))?;
  let base = needy_core::image::Digest::parse(&commit.base).map_err(|e| Error::Invalid(format!("image commit base: {e}")))?;
  Ok(needy_core::image::Commit { layer, base })
}

pub fn proto_image_commit(commit: needy_core::image::Commit) -> v1::ImageCommit {
  v1::ImageCommit {
    layer: commit.layer.to_string(),
    base: commit.base.to_string(),
  }
}

fn image_user(user: v1::ImageUser) -> needy_config::profile::User {
  needy_config::profile::User {
    name: user.name,
    uid: user.uid,
    gid: user.gid,
  }
}

pub(super) fn pinned_features(raw: HashMap<String, String>) -> Result<BTreeMap<String, String>, Error> {
  let mut out = BTreeMap::new();
  for (reference, digest) in raw {
    if reference.trim().is_empty() {
      return Err(Error::Invalid("pinned feature reference cannot be empty".into()));
    }
    needy_core::image::Digest::parse(&digest).map_err(|e| Error::Invalid(format!("pinned feature `{reference}` digest: {e}")))?;
    out.insert(reference, digest);
  }
  Ok(out)
}

impl TryFrom<v1::BuildImageRequest> for ImageBuild {
  type Error = Error;

  fn try_from(request: v1::BuildImageRequest) -> Result<Self, Error> {
    if request.image_yaml.trim().is_empty() {
      return Err(Error::Missing("image definition"));
    }
    Ok(Self {
      image_yaml: request.image_yaml,
      driver: request.driver,
      user: request.user.map(image_user),
      commit: request.commit.map(image_commit).transpose()?,
      pinned_features: pinned_features(request.pinned_features)?,
    })
  }
}

pub fn image_build(request: v1::BuildImageRequest) -> Result<ImageBuild, Error> {
  request.try_into()
}

impl From<BuildStepOutcome> for v1::BuildStepResult {
  fn from(s: BuildStepOutcome) -> Self {
    Self {
      key: s.key,
      layer: s.layer,
      cached: s.cached,
      ms: s.ms,
    }
  }
}

impl From<ImageBuildResult> for v1::BuildImageResponse {
  fn from(result: ImageBuildResult) -> Self {
    Self {
      snapshot: result.snapshot,
      commit_rebased: result.commit_rebased,
      steps: result.steps.into_iter().map(Into::into).collect(),
      resolved_features: result.resolved_features.into_iter().collect(),
    }
  }
}

pub fn proto_image_build(result: ImageBuildResult) -> v1::BuildImageResponse {
  result.into()
}

impl TryFrom<v1::StartImageEditRequest> for ImageEditStart {
  type Error = Error;

  fn try_from(request: v1::StartImageEditRequest) -> Result<Self, Error> {
    if request.image_name.trim().is_empty() {
      return Err(Error::Missing("image name"));
    }
    if request.image_yaml.trim().is_empty() {
      return Err(Error::Missing("image definition"));
    }
    Ok(Self {
      image_name: request.image_name,
      image_yaml: request.image_yaml,
      driver: request.driver,
      user: request.user.map(image_user),
      commit: request.commit.map(image_commit).transpose()?,
      tty: request.tty.map(Into::into),
      wait_for_attach: request.wait_for_attach,
      pinned_features: pinned_features(request.pinned_features)?,
    })
  }
}

pub fn image_edit_start(request: v1::StartImageEditRequest) -> Result<ImageEditStart, Error> {
  request.try_into()
}

impl From<ImageEditStarted> for v1::StartImageEditResponse {
  fn from(started: ImageEditStarted) -> Self {
    Self {
      session_id: started.session_id,
      commit_rebased: started.commit_rebased,
      resolved_features: started.resolved_features.into_iter().collect(),
    }
  }
}

pub fn proto_image_edit_started(started: ImageEditStarted) -> v1::StartImageEditResponse {
  started.into()
}

impl From<RecordedCommand> for v1::RecordedCommand {
  fn from(cmd: RecordedCommand) -> Self {
    Self {
      argv: cmd.argv,
      cwd: cmd.cwd,
    }
  }
}

impl From<ImageEditFinished> for v1::FinishImageEditResponse {
  fn from(finished: ImageEditFinished) -> Self {
    Self {
      commit: finished.commit.map(proto_image_commit),
      commands: finished.commands.into_iter().map(Into::into).collect(),
      exit: finished.exit.map(proto_exit),
    }
  }
}

pub fn proto_image_edit_finished(finished: ImageEditFinished) -> v1::FinishImageEditResponse {
  finished.into()
}

impl TryFrom<v1::RollbackImageRequest> for ImageRollback {
  type Error = Error;

  fn try_from(request: v1::RollbackImageRequest) -> Result<Self, Error> {
    if request.image_name.trim().is_empty() {
      return Err(Error::Missing("image name"));
    }
    if request.from_layer.trim().is_empty() {
      return Err(Error::Missing("current commit layer"));
    }
    if request.steps == 0 {
      return Err(Error::Invalid("rollback steps must be at least 1".into()));
    }
    Ok(Self {
      image_name: request.image_name,
      from_layer: request.from_layer,
      steps: request.steps,
    })
  }
}

pub fn image_rollback(request: v1::RollbackImageRequest) -> Result<ImageRollback, Error> {
  request.try_into()
}

impl From<ImageRollbackResult> for v1::RollbackImageResponse {
  fn from(result: ImageRollbackResult) -> Self {
    Self {
      layer: result.layer.unwrap_or_default(),
      base: result.base.unwrap_or_default(),
    }
  }
}

pub fn proto_image_rollback(result: ImageRollbackResult) -> v1::RollbackImageResponse {
  result.into()
}

impl TryFrom<v1::LogImageRequest> for ImageLog {
  type Error = Error;

  fn try_from(request: v1::LogImageRequest) -> Result<Self, Error> {
    if request.image_name.trim().is_empty() {
      return Err(Error::Missing("image name"));
    }
    Ok(Self {
      image_name: request.image_name,
      current_layer: (!request.current_layer.trim().is_empty()).then_some(request.current_layer),
    })
  }
}

pub fn image_log(request: v1::LogImageRequest) -> Result<ImageLog, Error> {
  request.try_into()
}

impl From<CommitHistoryEntry> for v1::CommitHistoryEntry {
  fn from(c: CommitHistoryEntry) -> Self {
    Self {
      layer: c.layer,
      parent: c.parent.unwrap_or_default(),
      base: c.base.unwrap_or_default(),
      commands: c.commands.into_iter().map(Into::into).collect(),
      time_ms: c.time_ms,
      active: c.active,
    }
  }
}

impl From<ImageLogResult> for v1::LogImageResponse {
  fn from(result: ImageLogResult) -> Self {
    Self {
      commits: result.commits.into_iter().map(Into::into).collect(),
    }
  }
}

pub fn proto_image_log(result: ImageLogResult) -> v1::LogImageResponse {
  result.into()
}

impl TryFrom<v1::ExportImageRequest> for ImageExport {
  type Error = Error;

  fn try_from(request: v1::ExportImageRequest) -> Result<Self, Error> {
    if request.image_yaml.trim().is_empty() {
      return Err(Error::Missing("image definition"));
    }
    Ok(Self {
      image_yaml: request.image_yaml,
      driver: request.driver,
      user: request.user.map(image_user),
      commit: request.commit.map(image_commit).transpose()?,
      pinned_features: pinned_features(request.pinned_features)?,
      tag: (!request.tag.trim().is_empty()).then_some(request.tag),
    })
  }
}

pub fn image_export(request: v1::ExportImageRequest) -> Result<ImageExport, Error> {
  request.try_into()
}
