//! Sessions: create, get, list and stop, through `api-server`'s `Sessions`
//! trait, and the registry of live and ended sessions.
//!
//! A session is registered while it is created, so it is listed as
//! `creating`. A create that fails unwinds what it did and is kept as a
//! failed session; a stopped session is kept as stopped, so `session ls`,
//! `session logs` and `report` still find it.

pub mod state;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::{Context, Result, bail};
use needy_api_server::control::{ControlError, NewSession, Route, SessionInfo, Sessions};
use needy_common::time::now_ms;
use needy_config::Source;
use needy_config::profile::{Profile, TraceSink};
use needy_config::validate::{self, Severity};
use needy_core::chaos::Injector;
use needy_core::event::{self, Event, ExecutorRef, SessionCreate, SessionFail, SessionStop};
use needy_core::intercept::ExecContext;
use needy_core::recipe::CompiledRecipe;
use needy_core::types::SessionState;
use needy_core_actor::host::vm;
use needy_core_executor::provider::InstanceSpec;
use needy_trace::Dispatch;
use needy_trace::sink::Sink;
use needy_trace::sink::jsonl::Jsonl;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::pool::Instance;
use crate::{Daemon, actor, failed, files, routing};
use state::{Refused, State};

/// Events a session's trace queue holds before it drops them.
// NOTICE: a burst larger than this while the sinks are slow is recorded as a
// `trace.gap`; 4096 envelopes are a few MiB at most.
const TRACE_QUEUE: usize = 4096;

/// A live session: being created, running, finished or stopping.
pub struct Session {
  pub id: String,
  pub name: String,
  pub group: String,
  pub profile_name: String,
  pub profile: Profile,
  pub created_ms: i64,
  pub dir: PathBuf,
  pub state: State,
  /// The actor's driving program.
  pub actor_command: Vec<String>,
  pub recipe: Option<CompiledRecipe>,
  pub injector: Arc<Injector>,
  pub trace: Dispatch,
  /// The session's executors by name, added as they are acquired.
  executors: Mutex<BTreeMap<String, Arc<Instance>>>,
  /// The session's tasks: the relays of its commands, and its actor's
  /// gateway and VM watch, which run under a child token.
  pub tasks: TaskTracker,
  pub cancel: CancellationToken,
}

impl Session {
  /// Records `event` in the session's trace.
  pub fn emit(&self, event: &Event) {
    emit(&self.trace, event);
  }

  pub fn executor(&self, name: &str) -> Option<Arc<Instance>> {
    self.lock_executors().get(name).cloned()
  }

  pub fn executors(&self) -> BTreeMap<String, Arc<Instance>> {
    self.lock_executors().clone()
  }

  pub fn info(&self) -> SessionInfo {
    SessionInfo {
      id: self.id.clone(),
      name: self.name.clone(),
      group: self.group.clone(),
      state: self.state.get(),
      executors: executor_refs(&self.executors()),
      created_ms: self.created_ms,
      profile: self.profile_name.clone(),
    }
  }

  fn lock_executors(&self) -> MutexGuard<'_, BTreeMap<String, Arc<Instance>>> {
    // NOTICE: each change is one insert, so a panic elsewhere leaves the map
    // whole.
    self.executors.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
  }
}

pub fn emit(trace: &Dispatch, event: &Event) {
  trace.emit(event.kind().as_str(), event.raw());
}

fn executor_refs(executors: &BTreeMap<String, Arc<Instance>>) -> Vec<ExecutorRef> {
  executors
    .iter()
    .map(|(name, instance)| ExecutorRef {
      name: name.clone(),
      runtime: instance.handle.runtime.into(),
      instance: instance.handle.id.clone(),
    })
    .collect()
}

// ------------------------------------------------------------- registry

/// Live sessions in the order they were created, and ended ones.
#[derive(Default)]
pub struct Registry {
  live: Mutex<Vec<Arc<Session>>>,
  /// Sessions that were stopped or failed to create, kept so `session ls`
  /// and `session logs` still find them.
  // TODO(persistence): this list lives in memory and grows until the daemon
  // exits; slice 9 keeps sessions in `state.db`.
  ended: Mutex<Vec<SessionInfo>>,
}

impl Registry {
  pub fn live(&self) -> Vec<Arc<Session>> {
    lock(&self.live).clone()
  }

  /// The live session `id` names.
  pub fn find(&self, id: &str) -> Result<Arc<Session>, ControlError> {
    pick(id, &lock(&self.live), |s| (&s.id, &s.name))?.ok_or_else(no_such_session)
  }

  /// The live or ended session `id` names; live sessions are looked at
  /// first.
  fn find_info(&self, id: &str) -> Result<SessionInfo, ControlError> {
    if let Some(session) = pick(id, &lock(&self.live), |s| (&s.id, &s.name))? {
      return Ok(session.info());
    }
    pick(id, &lock(&self.ended), |s| (&s.id, &s.name))?.ok_or_else(no_such_session)
  }

  fn list(&self) -> Vec<SessionInfo> {
    let mut sessions: Vec<_> = lock(&self.live).iter().map(|s| s.info()).collect();
    sessions.extend(lock(&self.ended).iter().cloned());
    sessions.sort_by_key(|s| s.created_ms);
    sessions
  }

  fn insert(&self, session: Arc<Session>) {
    lock(&self.live).push(session);
  }

  /// Moves session `info.id` from the live sessions to the ended ones.
  fn end(&self, info: SessionInfo) {
    lock(&self.live).retain(|s| s.id != info.id);
    lock(&self.ended).push(info);
  }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
  // NOTICE: the lists change by one push or retain, so a panic elsewhere
  // leaves them whole.
  mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn no_such_session() -> ControlError {
  ControlError::NotFound("no such session".into())
}

/// The one item `id` names: the one whose ID is `id`, else the only one whose
/// ID starts with `id` or whose name is `id`. An empty `id` names none, and a
/// prefix or name that fits several is an error.
fn pick<T: Clone>(id: &str, items: &[T], names: impl Fn(&T) -> (&str, &str)) -> Result<Option<T>, ControlError> {
  if id.is_empty() {
    return Ok(None);
  }
  if let Some(exact) = items.iter().find(|item| names(item).0 == id) {
    return Ok(Some(exact.clone()));
  }
  let matches: Vec<&T> = items
    .iter()
    .filter(|item| {
      let (item_id, name) = names(item);
      item_id.starts_with(id) || name == id
    })
    .collect();
  match matches.as_slice() {
    [] => Ok(None),
    [one] => Ok(Some((*one).clone())),
    _ => Err(ControlError::Invalid(format!("session {id} is ambiguous: {} sessions match", matches.len()))),
  }
}

// ------------------------------------------------------------ lifecycle

impl Daemon {
  /// Creates a session; a failure is kept as a failed session and recorded
  /// as `session.fail`.
  async fn create_session(&self, request: NewSession) -> Result<Arc<Session>> {
    let id = uuid::Uuid::new_v4().simple().to_string()[..12].to_string();
    let created_ms = now_ms();
    let loaded = load_profile(&id, &request);
    let trace = self.session_trace(&id, loaded.as_ref().ok().map(|(profile, _)| &profile.trace));
    let prepared =
      loaded.and_then(|(profile, base)| self.prepare(&id, &request, profile, &base, created_ms, trace.clone()).map(|s| (s, base)));
    let (session, base) = match prepared {
      Ok(prepared) => prepared,
      Err(error) => {
        self.record_failure(&id, &request, created_ms, &trace, &error).await;
        return Err(error);
      }
    };
    self.sessions.insert(session.clone());
    if let Err(error) = self.provision(&session, &request, &base).await {
      self.rollback(&session, &request, &error).await;
      return Err(error);
    }
    // An actor that exited already has finished its session.
    if let Err(refused) = session.state.advance(SessionState::Running) {
      tracing::debug!("session {id}: {refused}");
    }
    Ok(session)
  }

  /// The session before it acquires anything: its directory, profile and
  /// recipe.
  fn prepare(
    &self,
    id: &str,
    request: &NewSession,
    profile: Profile,
    base: &Path,
    created_ms: i64,
    trace: Dispatch,
  ) -> Result<Arc<Session>> {
    let dir = self.session_dir(id);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("profile.yaml"), &request.profile_yaml)?;
    let or_id = |value: &str| {
      if value.is_empty() {
        id.to_string()
      } else {
        value.to_string()
      }
    };
    let recipe = match &profile.actor.recipe {
      Some(reference) => {
        let recipe = needy_config::recipe::load(&Source::reference(reference, base))?;
        let host = needy_core_actor::host::detect::detect().unwrap_or_else(|e| {
          tracing::warn!("session {id}: cannot read this machine's CPU; the recipe skips host checks: {e}");
          Default::default()
        });
        let seed = u64::from_str_radix(&id[..8], 16).unwrap_or(0);
        let compiled = needy_core::recipe::compile(&recipe, &host, seed);
        for finding in &compiled.findings {
          tracing::warn!("session {id}: recipe {finding}");
        }
        Some(compiled)
      }
      None => None,
    };
    let actor_command = if request.actor_command.is_empty() {
      profile.actor.command.to_vec()
    } else {
      request.actor_command.clone()
    };
    let cancel = self.cancel.child_token();
    Ok(Arc::new(Session {
      id: id.to_string(),
      name: or_id(&request.name),
      group: or_id(&request.group),
      profile_name: request.profile_name.clone(),
      injector: Arc::new(Injector::new(&profile.chaos)),
      profile,
      created_ms,
      dir,
      state: State::new(),
      actor_command,
      recipe,
      trace,
      executors: Mutex::new(BTreeMap::new()),
      tasks: TaskTracker::new(),
      cancel,
    }))
  }

  /// Acquires the session's executors, copies files in and starts its actor.
  async fn provision(&self, session: &Arc<Session>, request: &NewSession, base: &Path) -> Result<()> {
    // Executors present the actor's hostname, so `uname -n` agrees on both sides.
    let hostname = session.recipe.as_ref().and_then(|r| r.hostname.clone());
    for (exec_name, config) in &session.profile.executors {
      let key = config.key.render(&[
        ("session", &session.id),
        ("group", &session.group),
        ("profile", &session.profile_name),
        ("rule", exec_name),
      ]);
      let spec = InstanceSpec {
        name: exec_name.clone(),
        key: format!("{exec_name}@{key}"),
        session: session.id.clone(),
        config: config.clone(),
        base: base.to_path_buf(),
        user: session.profile.user.clone(),
        hostname: hostname.clone(),
      };
      let instance = self.pool.acquire(spec).await.with_context(|| format!("executor {exec_name}"))?;
      session.lock_executors().insert(exec_name.clone(), instance);
    }
    session.emit(&Event::SessionCreate(SessionCreate {
      name: session.name.clone(),
      group: session.group.clone(),
      profile: session.profile_name.clone(),
      executors: executor_refs(&session.executors()),
    }));
    files::copy_in(session).await?;
    if request.start_actor {
      actor::start(self, session).await?;
    }
    Ok(())
  }

  /// Unwinds a create that failed after the session was registered.
  async fn rollback(&self, session: &Arc<Session>, request: &NewSession, error: &anyhow::Error) {
    if let Err(refused) = session.state.advance(SessionState::Failed) {
      tracing::debug!("session {}: {refused}", session.id);
    }
    session.cancel.cancel();
    session.tasks.close();
    session.tasks.wait().await;
    // TODO(8d): the instances in `session.executors()` that were acquired
    // before the failure are not released; slice 8d releases them here with
    // a regression test.
    self.record_failure(&session.id, request, session.created_ms, &session.trace, error).await;
  }

  /// Keeps a failed create as a failed session and records why.
  async fn record_failure(&self, id: &str, request: &NewSession, created_ms: i64, trace: &Dispatch, error: &anyhow::Error) {
    tracing::warn!("session {id} failed: {error:#}");
    let or_id = |value: &str| {
      if value.is_empty() {
        id.to_string()
      } else {
        value.to_string()
      }
    };
    self.sessions.end(SessionInfo {
      id: id.to_string(),
      name: or_id(&request.name),
      group: or_id(&request.group),
      state: SessionState::Failed,
      executors: Vec::new(),
      created_ms,
      profile: request.profile_name.clone(),
    });
    emit(
      trace,
      &Event::SessionFail(SessionFail {
        reason: format!("{error:#}"),
      }),
    );
    // `needy report` right after the failed create must find the reason.
    trace.flush().await;
  }

  /// Stops a running or finished session: its tasks, mirror-out, its
  /// executors, its trace.
  pub(crate) async fn stop_session(&self, session: &Arc<Session>) -> Result<(), ControlError> {
    let stopping = match session.state.advance(SessionState::Stopping) {
      Ok(_) => true,
      // The actor finished first and that state stays; the session still
      // holds its executors until it is stopped.
      Err(Refused {
        from: SessionState::Finished,
        ..
      }) => false,
      Err(refused) => return Err(ControlError::Failed(format!("session {}: {refused}", session.id))),
    };
    // The relays of the session's commands and its actor's tasks stop.
    session.cancel.cancel();
    session.tasks.close();
    session.tasks.wait().await;
    if let Err(e) = files::mirror_out(session).await {
      tracing::warn!("mirror-out for {}: {e:#}", session.id);
    }
    if stopping && let Err(refused) = session.state.advance(SessionState::Stopped) {
      tracing::warn!("session {}: {refused}", session.id);
    }
    for instance in session.executors().values() {
      self.pool.release(instance).await;
    }
    session.emit(&Event::SessionStop(SessionStop {}));
    // `needy report` right after the stop must see it, and the timing
    // summary is written now.
    session.trace.flush().await;
    // TODO(8c): the actor's sockets (`vsock` and its guest and gateway
    // ports) stay in the session's directory; slice 8c removes them here.
    self.sessions.end(session.info());
    Ok(())
  }

  fn session_dir(&self, id: &str) -> PathBuf {
    self.config.state_dir.join("sessions").join(id)
  }

  /// Starts the trace of session `id` with the sinks `config` selects; the
  /// default sinks when the profile could not be read.
  fn session_trace(&self, id: &str, config: Option<&needy_config::profile::Trace>) -> Dispatch {
    let default = needy_config::profile::Trace::default();
    let config = config.unwrap_or(&default);
    let mut sinks: Vec<Box<dyn Sink>> = Vec::new();
    // A sink listed twice is written once.
    let selected: BTreeSet<TraceSink> = config.sinks.iter().copied().collect();
    for sink in selected {
      match sink {
        TraceSink::Sqlite => sinks.push(Box::new(self.traces.sink())),
        TraceSink::Jsonl => {
          let path = self.session_dir(id).join("trace.jsonl");
          match Jsonl::create(&path) {
            Ok(jsonl) => sinks.push(Box::new(jsonl)),
            Err(e) => tracing::warn!("session {id}: the trace goes without JSONL: {e}"),
          }
        }
      }
    }
    Dispatch::new(
      id,
      sinks,
      needy_trace::Options {
        capacity: TRACE_QUEUE,
        version: event::SCHEMA_VERSION,
        timing: config.timing,
      },
    )
  }
}

/// Reads and checks the profile of a session about to be created, and the
/// directory its relative paths resolve against.
fn load_profile(id: &str, request: &NewSession) -> Result<(Profile, PathBuf)> {
  let base = if request.profile_dir.is_empty() {
    std::env::current_dir()?
  } else {
    PathBuf::from(&request.profile_dir)
  };
  let profile = needy_config::profile::load(&Source::text(request.profile_yaml.clone(), &base))?;
  let findings = validate::profile(&profile);
  for finding in findings.iter().filter(|f| f.severity == Severity::Warning) {
    tracing::warn!("session {id}: profile {finding}");
  }
  let errors: Vec<&str> = findings.iter().filter(|f| f.severity == Severity::Error).map(|f| f.message.as_str()).collect();
  if !errors.is_empty() {
    bail!("invalid profile: {}", errors.join("; "));
  }
  Ok((profile, base))
}

// ------------------------------------------------------------------ API

#[async_trait::async_trait]
impl Sessions for Daemon {
  async fn create(&self, request: NewSession) -> Result<SessionInfo, ControlError> {
    let session = self.create_session(request).await.map_err(failed)?;
    Ok(session.info())
  }

  async fn get(&self, id: &str) -> Result<SessionInfo, ControlError> {
    self.sessions.find_info(id)
  }

  async fn list(&self) -> Result<Vec<SessionInfo>, ControlError> {
    Ok(self.sessions.list())
  }

  async fn stop(&self, id: &str) -> Result<(), ControlError> {
    let session = self.sessions.find(id)?;
    self.stop_session(&session).await
  }

  async fn logs(&self, id: &str) -> Result<Vec<u8>, ControlError> {
    let info = self.sessions.find_info(id)?;
    // Firecracker writes the guest's serial console here.
    let path = vm::console(&self.session_dir(&info.id));
    // TODO(session-logs): follow a running actor's console (`--follow`); not
    // needed by the QA item, which asks for the output so far.
    match tokio::fs::read(&path).await {
      Ok(bytes) => Ok(bytes),
      Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(ControlError::NotFound(format!("session {id} has no actor console"))),
      Err(e) => Err(ControlError::Internal(format!("cannot read {}: {e}", path.display()))),
    }
  }

  async fn decide(&self, session: &str, ctx: ExecContext) -> Result<Route, ControlError> {
    let session = self.sessions.find(session)?;
    Ok(routing::route(&session, ctx).await)
  }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
