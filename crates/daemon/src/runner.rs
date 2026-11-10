//! The runner role: a machine that joined a control plane. On its first
//! start it joins with a join token and keeps the credential it gets in
//! `runner.json` in its state directory (mode 0600); later starts reuse
//! that credential. It sends heartbeats while it runs, and serves the
//! executor role on its own listener as `needy daemon --mode executor` does.
//
// TODO(runner-dial-out): the control plane still dials this runner's
// executor listener through a profile's `remote` executor; slice 4 of the
// runners-and-runs design replaces that with a control stream the runner
// opens, and the listener goes away.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use needy_api_client::proto::v1::{HeartbeatRequest, JoinRequest};
use needy_api_client::{Client, Kind};
use needy_api_server::protocol::domain::proto_runner_role;
use needy_core::types::RunnerRole;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

/// Where a runner keeps its credential, in its state directory.
const ENROLLMENT: &str = "runner.json";

// NOTICE: how long a runner waits to try again when the control plane does
// not answer a heartbeat. Shorter than the control plane's interval (15 s by
// default), so a runner shows as online soon after the control plane or a
// tunnel comes back; one small call every 5 s per waiting runner costs
// nothing.
const RETRY: Duration = Duration::from_secs(5);

/// How a runner starts.
#[derive(Debug, Clone)]
pub struct Config {
  /// `runner.json`.
  pub state_dir: PathBuf,
  /// The control plane's endpoint; the one stored at the join when `None`.
  pub control: Option<String>,
  /// Needed on the first start only.
  pub join_token: Option<String>,
  pub name: String,
  // TODO(runner-roles): roles are declared by flags; detecting them (such as
  // `/dev/kvm` for the actor role) is a follow-up of slice 3.
  pub roles: BTreeSet<RunnerRole>,
  pub labels: BTreeMap<String, String>,
}

/// What a runner keeps between starts.
#[derive(Serialize, Deserialize)]
struct Enrollment {
  /// The endpoint it joined through.
  control: String,
  name: String,
  credential: String,
}

/// A runner that joined.
pub struct Runner {
  path: PathBuf,
  control: String,
  enrollment: Enrollment,
  roles: BTreeSet<RunnerRole>,
  labels: BTreeMap<String, String>,
  /// Connected on the first heartbeat, so a control plane that is not up
  /// yet only delays heartbeats.
  client: Option<Client>,
  /// As the control plane last said; `None` until it said.
  interval: Option<Duration>,
}

impl Runner {
  /// Loads the stored credential, or joins with the join token when there
  /// is none.
  pub async fn start(config: Config) -> Result<Self> {
    let path = config.state_dir.join(ENROLLMENT);
    let (enrollment, interval) = match load(&path)? {
      Some(stored) => {
        if stored.name != config.name {
          bail!(
            "this runner joined as {}, not {}; pass --name {} or delete {} to join again",
            stored.name,
            config.name,
            stored.name,
            path.display()
          );
        }
        (stored, None)
      }
      None => {
        let Some(join_token) = &config.join_token else {
          bail!("this runner has not joined yet: give it a join token with --join-token-env or --join-token-file");
        };
        let Some(control) = &config.control else {
          bail!("this runner has not joined yet: give it the control plane's endpoint with --join");
        };
        join(&config, control, join_token, &path).await?
      }
    };
    let control = config.control.clone().unwrap_or_else(|| enrollment.control.clone());
    Ok(Self {
      path,
      control,
      enrollment,
      roles: config.roles,
      labels: config.labels,
      client: None,
      interval,
    })
  }

  /// The runner's name.
  pub fn name(&self) -> &str {
    &self.enrollment.name
  }

  /// Sends one heartbeat. A rotated credential in the answer is stored
  /// first, then used from the next call on.
  pub async fn beat(&mut self) -> Result<()> {
    let client = match &self.client {
      Some(client) => client.clone(),
      None => {
        let client = Client::connect(&self.control).await.with_context(|| format!("cannot reach the control plane at {}", self.control))?;
        let client = client.with_token(&self.enrollment.credential)?;
        self.client = Some(client.clone());
        client
      }
    };
    let request = HeartbeatRequest {
      roles: self.roles.iter().map(|&role| proto_runner_role(role) as i32).collect(),
      labels: self.labels.clone().into_iter().collect(),
    };
    let beat = client.runners().heartbeat(request).await.map_err(needy_api_client::Error::from)?.into_inner();
    self.interval = interval(beat.heartbeat_interval_ms);
    if !beat.credential.is_empty() {
      self.enrollment.credential = beat.credential;
      store(&self.path, &self.enrollment)?;
      self.client = Some(client.with_token(&self.enrollment.credential)?);
      tracing::info!("runner {}: stored a rotated credential", self.enrollment.name);
    }
    Ok(())
  }

  /// Sends heartbeats until `stop`, at the interval the control plane
  /// asks for. Fails when the control plane refuses the credential: the
  /// runner was removed, or its credential rotated away.
  pub async fn run(mut self, stop: CancellationToken) -> Result<()> {
    loop {
      let before = self.enrollment.credential.clone();
      let wait = match self.beat().await {
        // A rotated credential is used at once, which retires the old one
        // instead of leaving both valid for an interval.
        Ok(()) if self.enrollment.credential != before => Duration::ZERO,
        Ok(()) => self.interval.unwrap_or(RETRY),
        Err(e) if refused(&e) => {
          bail!(
            "the control plane refused this runner's credential: the runner was removed or its credential replaced; \
             delete {} and join again with a new join token",
            self.path.display()
          )
        }
        Err(e) => {
          tracing::warn!("runner {}: heartbeat: {e:#}", self.enrollment.name);
          // The connection is made again for the next heartbeat.
          self.client = None;
          RETRY
        }
      };
      tokio::select! {
        () = stop.cancelled() => return Ok(()),
        () = tokio::time::sleep(wait) => {}
      }
    }
  }
}

/// Whether the control plane refused the runner's credential, rather than
/// being unreachable.
fn refused(error: &anyhow::Error) -> bool {
  error.downcast_ref::<needy_api_client::Error>().is_some_and(|e| e.kind() == Kind::Unauthenticated)
}

/// Joins the control plane and stores the credential before anything
/// else, so a runner that stops right after does not lose it.
async fn join(config: &Config, control: &str, join_token: &str, path: &Path) -> Result<(Enrollment, Option<Duration>)> {
  let client = Client::connect(control).await.with_context(|| format!("cannot reach the control plane at {control}"))?;
  let request = JoinRequest {
    join_token: join_token.to_string(),
    name: config.name.clone(),
    roles: config.roles.iter().map(|&role| proto_runner_role(role) as i32).collect(),
    labels: config.labels.clone().into_iter().collect(),
  };
  let joined = client.runners().join(request).await.map_err(needy_api_client::Error::from).context("cannot join the control plane")?;
  let joined = joined.into_inner();
  let enrollment = Enrollment {
    control: control.to_string(),
    name: config.name.clone(),
    credential: joined.credential,
  };
  store(path, &enrollment)?;
  tracing::info!("joined the control plane at {control} as runner {}", config.name);
  Ok((enrollment, interval(joined.heartbeat_interval_ms)))
}

/// The heartbeat interval the control plane asked for; none for a zero or
/// negative one, which would send heartbeats without pause.
fn interval(ms: i64) -> Option<Duration> {
  u64::try_from(ms).ok().filter(|&ms| ms > 0).map(Duration::from_millis)
}

fn load(path: &Path) -> Result<Option<Enrollment>> {
  match std::fs::read_to_string(path) {
    Ok(text) => Ok(Some(serde_json::from_str(&text).with_context(|| format!("cannot read {}", path.display()))?)),
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
    Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
  }
}

/// Writes `enrollment` to a temporary file readable by this user only, then
/// renames it over `path`, so a crash leaves the old credential or the new
/// one, never half of one.
fn store(path: &Path, enrollment: &Enrollment) -> Result<()> {
  let dir = path.parent().context("the enrollment has no directory")?;
  #[cfg(unix)]
  needy_api_server::create_private_dir(dir).with_context(|| format!("cannot create {}", dir.display()))?;
  #[cfg(not(unix))]
  std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
  let partial = path.with_extension("json.partial");
  let mut options = std::fs::OpenOptions::new();
  options.write(true).create(true).truncate(true);
  #[cfg(unix)]
  {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600);
  }
  // TODO(windows): the file keeps the directory's ACL on Windows; a runner
  // there relies on its state directory being private to the user.
  let write = || -> std::io::Result<()> {
    use std::io::Write as _;
    let mut file = options.open(&partial)?;
    file.write_all(&serde_json::to_vec_pretty(enrollment)?)?;
    file.sync_all()?;
    std::fs::rename(&partial, path)
  };
  write().with_context(|| format!("cannot write {}", path.display()))
}

/// Joins or loads the credential, then serves the executor role on `listen`
/// (`executor_token` for its clients) and sends heartbeats, until a
/// termination signal. Ends with an error when the control plane refuses
/// the runner's credential; the executor's processes are stopped first.
pub async fn serve(config: Config, listen: &str, executor_token: Option<String>) -> Result<()> {
  let runner = Runner::start(config).await?;
  let name = runner.name().to_string();
  // The first heartbeat goes out at once, so a refused credential stops
  // the executor role right after it started.
  let refusal = CancellationToken::new();
  let stop_beats = CancellationToken::new();
  let beats = {
    let refusal = refusal.clone();
    let stop = stop_beats.clone();
    tokio::spawn(async move {
      let ended = runner.run(stop).await;
      if ended.is_err() {
        refusal.cancel();
      }
      ended
    })
  };
  tracing::info!("runner {name} serving the executor role");
  let served = crate::executor::serve_until(listen, name, executor_token, refusal).await;
  stop_beats.cancel();
  let beaten = beats.await.context("the heartbeat task failed")?;
  served?;
  beaten
}

#[cfg(all(test, unix))]
#[path = "runner_test.rs"]
mod tests;
