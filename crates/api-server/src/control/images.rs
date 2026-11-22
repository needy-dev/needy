//! Domain types and trait for `ImageService`.

use std::collections::BTreeMap;

use needy_common::process::Exit;

use super::{Chunks, Result, WindowSize};

/// A request to build an image definition's layers and return its snapshot.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImageBuild {
  /// Self-contained image definition YAML (its `extends` chain merged).
  pub image_yaml: String,
  /// Actor driver name; empty selects this host's VM driver.
  pub driver: String,
  /// Profile user for build plan cache keys; `None` uses the default user.
  pub user: Option<needy_config::profile::User>,
  /// Optional commit layer from the image's lockfile, stacked on top.
  pub commit: Option<needy_core::image::Commit>,
  /// Pinned feature digests (`canonical_ref -> sha256:<hex>`) from the
  /// image's lockfile.
  pub pinned_features: BTreeMap<String, String>,
}

/// One step of an image build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildStepOutcome {
  pub key: String,
  pub layer: String,
  pub cached: bool,
  pub ms: u64,
}

/// Result of [`Images::build`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageBuildResult {
  pub snapshot: String,
  pub commit_rebased: bool,
  pub steps: Vec<BuildStepOutcome>,
  /// Resolved feature digests (`canonical_ref -> sha256:<hex>`) for the CLI
  /// to record in the image's lockfile.
  pub resolved_features: BTreeMap<String, String>,
}

/// A request to start an interactive edit session for an image.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImageEditStart {
  /// Stable identifier for the image (per-image edit lock and commit history key).
  pub image_name: String,
  /// Self-contained image definition YAML (its `extends` chain merged).
  pub image_yaml: String,
  /// Actor driver name; empty selects this host's VM driver.
  pub driver: String,
  /// Profile user for build plan cache keys; `None` uses the default user.
  pub user: Option<needy_config::profile::User>,
  /// Current commit from the image's lockfile, if any.
  pub commit: Option<needy_core::image::Commit>,
  /// Terminal size for the interactive shell.
  pub tty: Option<WindowSize>,
  /// Wait for the client to attach its terminal before starting the shell.
  pub wait_for_attach: bool,
  /// Pinned feature digests (`canonical_ref -> sha256:<hex>`) from the
  /// image's lockfile.
  pub pinned_features: BTreeMap<String, String>,
}

/// Result of [`Images::start_edit`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageEditStarted {
  pub session_id: String,
  pub commit_rebased: bool,
  /// Resolved feature digests (`canonical_ref -> sha256:<hex>`) for the CLI
  /// to record in the image's lockfile.
  pub resolved_features: BTreeMap<String, String>,
}

/// One program an edit session executed, as decided by the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedCommand {
  pub argv: Vec<String>,
  pub cwd: String,
}

/// Result of [`Images::finish_edit`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageEditFinished {
  /// The new commit, or `None` if the session exited non-zero, was aborted,
  /// or uploaded no upper layer.
  pub commit: Option<needy_core::image::Commit>,
  /// Programs executed during the edit session, in order.
  pub commands: Vec<RecordedCommand>,
  /// The shell's exit status, if reported.
  pub exit: Option<Exit>,
}

/// A request to step an image's commit pointer back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRollback {
  pub image_name: String,
  pub from_layer: String,
  pub steps: u32,
}

/// Result of [`Images::rollback`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRollbackResult {
  /// Target commit layer digest, or `None` when rolled back past the first commit.
  pub layer: Option<String>,
  /// Original base plan top digest the target commit was made on, or `None`
  /// when rolled back past the first commit or when the commit predates base
  /// persistence.
  pub base: Option<String>,
}

/// A request to inspect an image's commit history in the layer store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageLog {
  /// Stable identifier for the image (commit history key).
  pub image_name: String,
  /// Current commit layer digest from the lockfile, if any, used to mark the
  /// active commit.
  pub current_layer: Option<String>,
}

/// One recorded commit in an image's history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitHistoryEntry {
  pub layer: String,
  pub parent: Option<String>,
  pub base: Option<String>,
  pub commands: Vec<RecordedCommand>,
  pub time_ms: i64,
  pub active: bool,
}

/// Result of [`Images::log`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageLogResult {
  pub commits: Vec<CommitHistoryEntry>,
}

/// A request to build/resolve an image definition and stream it as an
/// OCI/Docker tar archive.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImageExport {
  /// Self-contained image definition YAML (its `extends` chain merged).
  pub image_yaml: String,
  /// Actor driver name; empty selects this host's VM driver.
  pub driver: String,
  /// Profile user for build plan cache keys; `None` uses the default user.
  pub user: Option<needy_config::profile::User>,
  /// Optional commit layer from the image's lockfile, stacked on top.
  pub commit: Option<needy_core::image::Commit>,
  /// Pinned feature digests (`canonical_ref -> sha256:<hex>`) from the
  /// image's lockfile.
  pub pinned_features: BTreeMap<String, String>,
  /// Optional `repository:tag` embedded in the exported archive's manifest.
  pub tag: Option<String>,
}

/// Building image definitions, interactive edit sessions, commit history,
/// rollback, and OCI archive export.
#[async_trait::async_trait]
pub trait Images: Send + Sync + 'static {
  async fn build(&self, request: ImageBuild) -> Result<ImageBuildResult>;
  async fn start_edit(&self, request: ImageEditStart) -> Result<ImageEditStarted>;
  async fn finish_edit(&self, session_id: &str, abort: bool) -> Result<ImageEditFinished>;
  async fn rollback(&self, request: ImageRollback) -> Result<ImageRollbackResult>;
  async fn log(&self, request: ImageLog) -> Result<ImageLogResult>;
  async fn export(&self, request: ImageExport) -> Result<Chunks>;
}
