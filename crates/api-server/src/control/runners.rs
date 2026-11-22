//! Domain types and trait for `RunnerService`.

use std::collections::{BTreeMap, BTreeSet};
use std::pin::Pin;

use needy_core::types::{Capabilities, RunnerRole};
use tokio_stream::Stream;

use super::{Credential, Result};

/// A join token to create.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewJoinToken {
  pub ttl_ms: i64,
  pub uses: u32,
  /// The roles a runner may declare; empty allows every role.
  pub roles: BTreeSet<RunnerRole>,
  /// Labels every runner that joins with it gets.
  pub labels: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinTokenState {
  Valid,
  Expired,
  UsedUp,
  Revoked,
}

/// A join token without its secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinTokenInfo {
  pub id: String,
  pub roles: BTreeSet<RunnerRole>,
  pub labels: BTreeMap<String, String>,
  pub created_ms: i64,
  pub expires_ms: i64,
  pub uses_left: u32,
  pub state: JoinTokenState,
}

/// A join token just created, with its secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedJoinToken {
  pub info: JoinTokenInfo,
  pub token: String,
}

/// What a runner declares when it joins and at each heartbeat.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunnerReport {
  /// What it can host; its roles must be within its join token's.
  pub capabilities: Capabilities,
  pub labels: BTreeMap<String, String>,
  /// Its machine; heartbeats carry it, a join does not.
  pub host: Option<RunnerHost>,
}

/// A runner's machine.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunnerHost {
  /// As `std::env::consts` names them: `linux`, `macos`; `x86_64`, `aarch64`.
  pub os: String,
  pub arch: String,
  /// Its CPU and the CPUID entries its KVM supports, for recipe checks.
  pub cpu: needy_core::recipe::HostInfo,
  /// The home directory of the user the runner runs as: `~` in a native
  /// executor's paths there. Empty when unknown.
  pub home: String,
}

/// A runner asking to join.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Joining {
  pub join_token: String,
  pub name: String,
  pub report: RunnerReport,
}

/// A runner that joined, with its credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Joined {
  pub runner: RunnerInfo,
  pub credential: String,
  pub heartbeat_interval_ms: i64,
}

/// The answer to a heartbeat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Beat {
  /// A new credential after a rotation.
  pub credential: Option<String>,
  pub heartbeat_interval_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunnerStatus {
  Online,
  Offline,
  Removed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerInfo {
  pub name: String,
  /// As it last reported them.
  pub capabilities: Capabilities,
  pub labels: BTreeMap<String, String>,
  pub status: RunnerStatus,
  pub created_ms: i64,
  pub last_seen_ms: i64,
  pub rotating: bool,
  pub join_token: String,
  /// Its machine, once a heartbeat reported it to this control plane.
  pub host: Option<RunnerHost>,
}

/// What a runner sends on one of its streams; an error is the stream
/// failing, such as the runner's connection ending.
pub type FromRunner<T> = Pin<Box<dyn Stream<Item = std::result::Result<T, tonic::Status>> + Send>>;
/// What the control plane sends a runner on one of its streams.
pub type ToRunner<T> = Pin<Box<dyn Stream<Item = T> + Send>>;

/// The control plane's runner registry, and the streams runners open to it.
#[async_trait::async_trait]
pub trait Runners: Send + Sync + 'static {
  async fn create_join_token(&self, request: NewJoinToken) -> Result<CreatedJoinToken>;
  async fn join_tokens(&self) -> Result<Vec<JoinTokenInfo>>;
  async fn revoke_join_token(&self, id: &str) -> Result<()>;
  async fn join(&self, request: Joining) -> Result<Joined>;
  async fn heartbeat(&self, runner: &str, report: RunnerReport) -> Result<Beat>;
  /// Every runner, including removed ones.
  async fn runners(&self) -> Result<Vec<RunnerInfo>>;
  async fn rotate(&self, runner: &str) -> Result<()>;
  async fn remove(&self, runner: &str) -> Result<()>;
  /// Whose credential a bearer token is. The middleware calls it for every
  /// token that is not the control token. Backend failures are errors;
  /// invalid credentials are `Credential::Refused`.
  async fn credential(&self, token: &str) -> Result<Credential>;
  /// The control stream of `runner`: `results` are what it answers, the
  /// returned stream the tasks it gets. It replaces an earlier control
  /// stream of the same runner.
  // NOTICE: a runner's streams carry the exec protocol's own messages
  // (`ExecRequest`, `ExecResponse`) unchanged, so terminal behavior,
  // backpressure and refusals work as on any executor; converting them to
  // domain types here and back in the daemon would only copy them.
  async fn control(
    &self,
    runner: &str,
    results: FromRunner<needy_api_proto::v1::ControlRequest>,
  ) -> Result<ToRunner<needy_api_proto::v1::ControlResponse>>;
  /// The stream `runner` opened for its exec task `task`: `responses` are
  /// the exec's responses, the returned stream its requests.
  async fn task(
    &self,
    runner: &str,
    task: u64,
    responses: FromRunner<needy_api_proto::v1::TaskRequest>,
  ) -> Result<ToRunner<needy_api_proto::v1::TaskResponse>>;
}
