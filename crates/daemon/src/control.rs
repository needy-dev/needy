//! The control daemon: sessions, executor instances, routing, the actor's
//! services and its gateway.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use needy_api_client::exec::{self as client_exec, ExecStream};
use needy_api_client::files::{self, Target as FilesTarget};
use needy_api_client::proto::v1::{KillProcessRequest, ListProcessesRequest, Origin as WireOrigin};
use needy_api_server::control::{
  Caller, ControlError, ExecInput, ExecOpen, ExecOutput, ExecSpec, Guest, GuestConfig, GuestCopy, GuestFault, GuestMount, GuestSensor,
  GuestUser, Inputs, NewSession, Outputs, ProcessInfo, Processes, Route, SessionInfo, Sessions, Target,
};
use needy_api_server::protocol::domain;
use needy_api_server::{Health, Listen, Server};
use needy_common::process::Exit;
use needy_common::time::now_ms;
use needy_config::Source;
use needy_config::profile::{FaultKind, FileMode, OnFull, Profile, Runtime, Side, TraceSink};
use needy_config::validate::{self, Severity};
use needy_core::chaos::Injector;
use needy_core::chaos::sftp::SftpTap;
use needy_core::event::{
  self, ActorConfig, ActorExit, ActorStart, ActorVmExit, Event, ExecDecide, ExecExit, ExecStart, FileEnd, FilesCopyIn, FilesMirrorOut,
  SessionCreate, SessionFail, SessionStop,
};
use needy_core::intercept::{self, ExecContext};
use needy_core::recipe::CompiledRecipe;
use needy_core::rules::{self, Rules};
use needy_core::types::{Decision, Network, Origin, Outcome, Placement, SessionState};
use tokio::sync::{Semaphore, mpsc};
use tokio_stream::StreamExt as _;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;

use needy_core_actor::host::{gateway, vm};
use needy_core_executor::provider::{self, Docker, InstanceHandle, InstanceSpec, Native, Provider, Remote};
use needy_trace::Dispatch;
use needy_trace::sink::Sink;
use needy_trace::sink::jsonl::Jsonl;
use needy_trace::sink::sqlite::Store;

/// Ports the actor reaches on the host over vsock.
pub const GUEST_PORT: u32 = 7000;
pub const GATEWAY_PORT: u32 = 7001;

/// Events a session's trace queue holds before it drops them.
// NOTICE: a burst larger than this while the sinks are slow is recorded as a
// `trace.gap`; 4096 envelopes are a few MiB at most.
const TRACE_QUEUE: usize = 4096;

pub struct Instance {
  pub name: String,
  pub handle: InstanceHandle,
  /// The provider that created the instance and destroys it.
  provider: Arc<dyn Provider>,
  pub path_map: BTreeMap<String, String>,
  slots: Option<Arc<Semaphore>>,
  on_full: OnFull,
  refs: AtomicUsize,
}

pub struct Session {
  pub id: String,
  pub name: String,
  pub group: String,
  pub profile_name: String,
  pub profile: Profile,
  pub created_ms: i64,
  pub dir: PathBuf,
  pub executors: BTreeMap<String, Arc<Instance>>,
  pub state: Mutex<SessionState>,
  pub actor_command: Vec<String>,
  pub recipe: Option<CompiledRecipe>,
  pub injector: Arc<Injector>,
  pub trace: Dispatch,
}

impl Session {
  /// Records `event` in the session's trace.
  fn emit(&self, event: &Event) {
    emit(&self.trace, event);
  }
}

fn emit(trace: &Dispatch, event: &Event) {
  trace.emit(event.kind().as_str(), event.raw());
}

pub struct Daemon {
  pub state_dir: PathBuf,
  pub actor_image: PathBuf,
  /// `trace.db`, shared by the sessions' SQLite sinks.
  traces: Store,
  sessions: Mutex<HashMap<String, Arc<Session>>>,
  /// Sessions that were stopped or failed to create, kept so `session ls`
  /// and `session logs` still find them.
  // TODO(persistence): this list lives in memory and grows until the daemon
  // exits; slice 9 keeps sessions in `state.db`.
  ended: Mutex<Vec<SessionInfo>>,
  pools: tokio::sync::Mutex<HashMap<String, Arc<Instance>>>,
  procs: Mutex<HashMap<String, Arc<Instance>>>,
  local: Arc<Instance>,
  native: Arc<Native>,
  remote: Arc<Remote>,
  /// `None` when the Docker client could not be set up (a bad `DOCKER_HOST`).
  docker: Option<Arc<Docker>>,
}

/// A failure of the daemon's own work, such as an invalid profile or a
/// copy-in that cannot be packed.
fn failed(e: anyhow::Error) -> ControlError {
  ControlError::Failed(format!("{e:#}"))
}

/// An executor's error, relayed with the same meaning.
fn relayed(e: needy_api_client::Error) -> ControlError {
  use needy_api_client::Kind;
  let message = e.to_string();
  match e.kind() {
    Kind::Denied => ControlError::denied(message),
    // The executor refused the daemon's own token; to the daemon's caller
    // the executor is unavailable, not the command refused.
    Kind::Unauthenticated => ControlError::Unavailable(message),
    Kind::NotFound => ControlError::NotFound(message),
    Kind::Invalid => ControlError::Invalid(message),
    Kind::Failed => ControlError::Failed(message),
    Kind::Exhausted => ControlError::Exhausted(message),
    Kind::Unavailable => ControlError::Unavailable(message),
    Kind::Other => ControlError::Internal(message),
  }
}

impl Daemon {
  /// Starts the control plane. `state_dir` holds the trace and sessions;
  /// `runtime_dir` holds the local executor's socket.
  pub async fn start(state_dir: PathBuf, runtime_dir: PathBuf, actor_image: PathBuf, docker_image: String) -> Result<Arc<Self>> {
    let sessions_dir = state_dir.join("sessions");
    std::fs::create_dir_all(&sessions_dir).with_context(|| format!("cannot create {}", sessions_dir.display()))?;
    let trace_db = state_dir.join("trace.db");
    let traces = Store::open(&trace_db).await.with_context(|| format!("cannot open {}", trace_db.display()))?;
    // This machine is always available as the `local` executor.
    let local_sock = runtime_dir.join("local.sock");
    let service = needy_core_executor::service::ExecutorService {
      host: needy_core_executor::process::Host::default(),
      name: "local".into(),
    };
    let health = Health {
      instance_id: uuid::Uuid::new_v4().to_string(),
      role: "executor",
    };
    // TODO(8i): a socket that cannot bind (a path over `sun_path`) now
    // fails start-up here with its path; slice 8i adds the daemon's
    // regression test.
    let local_server = Server::new(health, Caller::Cli).process_service(service).bind(&[Listen::Unix(local_sock.clone())]).await?;
    // TODO(daemon): the local executor serves until the daemon exits; slice
    // 8 gives the daemon a token that `Daemon::shutdown` cancels.
    tokio::spawn(async move {
      if let Err(e) = local_server.serve(CancellationToken::new()).await {
        tracing::error!("local executor: {e:#}");
      }
    });
    let local_client = provider::connect(&format!("unix:{}", local_sock.display())).await?;
    let native = Arc::new(Native::new(local_client));
    let local = Arc::new(Instance {
      name: "local".into(),
      handle: native.machine(),
      provider: native.clone(),
      path_map: BTreeMap::new(),
      slots: None,
      on_full: OnFull::Wait,
      refs: AtomicUsize::new(1),
    });
    let docker = match Docker::new(provider::docker::Options {
      image: docker_image,
      state_dir: state_dir.clone(),
      allow: Vec::new(),
    }) {
      Ok(docker) => Some(Arc::new(docker)),
      Err(e) => {
        tracing::warn!("docker executors are unavailable: {e}");
        None
      }
    };
    Ok(Arc::new(Self {
      state_dir,
      actor_image,
      traces,
      sessions: Mutex::new(HashMap::new()),
      ended: Mutex::new(Vec::new()),
      pools: tokio::sync::Mutex::new(HashMap::new()),
      procs: Mutex::new(HashMap::new()),
      local,
      native,
      remote: Arc::new(Remote),
      docker,
    }))
  }

  /// Serves the control API on `listen` until a termination signal.
  pub async fn serve(self: &Arc<Self>, listen: &str, token: Option<String>) -> Result<()> {
    let cancel = needy_api_server::termination::token().context("cannot handle termination signals")?;
    let api = Arc::new(Api {
      daemon: self.clone(),
    });
    let health = Health {
      instance_id: uuid::Uuid::new_v4().to_string(),
      role: "control",
    };
    let server = Server::new(health, Caller::Cli).token(token).sessions(api.clone()).processes(api).bind(&[Listen::parse(listen)]).await?;
    tracing::info!("control daemon listening on {listen}");
    server.serve(cancel).await?;
    Ok(())
  }

  fn session(&self, id: &str) -> Option<Arc<Session>> {
    let sessions = self.sessions.lock().unwrap();
    sessions.get(id).cloned().or_else(|| {
      let matches: Vec<_> = sessions.values().filter(|s| s.id.starts_with(id) || s.name == id).cloned().collect();
      (matches.len() == 1).then(|| matches[0].clone())
    })
  }

  /// The directory of a running, finished, stopped or failed session, by ID,
  /// ID prefix or name.
  fn session_dir(&self, id: &str) -> Option<PathBuf> {
    if let Some(session) = self.session(id) {
      return Some(session.dir.clone());
    }
    let ended = self.ended.lock().unwrap();
    let matches: Vec<_> = ended.iter().filter(|s| s.id.starts_with(id) || s.name == id).collect();
    (matches.len() == 1).then(|| self.state_dir.join("sessions").join(&matches[0].id))
  }

  /// Starts the trace of session `id` with the sinks `config` selects; the
  /// default sinks when the profile could not be read.
  fn session_trace(&self, id: &str, config: Option<&needy_config::profile::Trace>) -> Dispatch {
    let default = needy_config::profile::Trace::default();
    let config = config.unwrap_or(&default);
    let mut sinks: Vec<Box<dyn Sink>> = Vec::new();
    // A sink listed twice is written once.
    let selected: std::collections::BTreeSet<TraceSink> = config.sinks.iter().copied().collect();
    for sink in selected {
      match sink {
        TraceSink::Sqlite => sinks.push(Box::new(self.traces.sink())),
        TraceSink::Jsonl => {
          let path = self.state_dir.join("sessions").join(id).join("trace.jsonl");
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

  /// Creates a session; a failure is kept as a failed session and recorded
  /// as `session.fail`.
  async fn create_session(self: &Arc<Self>, request: NewSession) -> Result<Arc<Session>> {
    let id = uuid::Uuid::new_v4().simple().to_string()[..12].to_string();
    let created_ms = now_ms();
    let loaded = load_profile(&id, &request);
    let trace = self.session_trace(&id, loaded.as_ref().ok().map(|(profile, _)| &profile.trace));
    let created = match loaded {
      Ok((profile, base)) => self.try_create_session(&id, &request, profile, base, trace.clone()).await,
      Err(error) => Err(error),
    };
    match created {
      Ok(session) => Ok(session),
      Err(error) => {
        // TODO(8d): instances acquired before the failure are not released;
        // slice 8d fixes that with a regression test.
        self.sessions.lock().unwrap().remove(&id);
        tracing::warn!("session {id} failed: {error:#}");
        let or_id = |value: &str| {
          if value.is_empty() {
            id.clone()
          } else {
            value.to_string()
          }
        };
        self.ended.lock().unwrap().push(SessionInfo {
          id: id.clone(),
          name: or_id(&request.name),
          group: or_id(&request.group),
          state: SessionState::Failed,
          executors: Vec::new(),
          created_ms,
          profile: request.profile_name.clone(),
        });
        emit(
          &trace,
          &Event::SessionFail(SessionFail {
            reason: format!("{error:#}"),
          }),
        );
        // `needy report` right after the failed create must find the reason.
        trace.flush().await;
        Err(error)
      }
    }
  }

  async fn try_create_session(
    self: &Arc<Self>,
    id: &str,
    request: &NewSession,
    profile: Profile,
    base: PathBuf,
    trace: Dispatch,
  ) -> Result<Arc<Session>> {
    let id = id.to_string();
    let dir = self.state_dir.join("sessions").join(&id);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("profile.yaml"), &request.profile_yaml)?;
    let group = if request.group.is_empty() {
      id.clone()
    } else {
      request.group.clone()
    };
    let name = if request.name.is_empty() {
      id.clone()
    } else {
      request.name.clone()
    };
    let profile_name = request.profile_name.clone();
    let recipe = match &profile.actor.recipe {
      Some(reference) => {
        let recipe = needy_config::recipe::load(&Source::reference(reference, &base))?;
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
    // Executors present the actor's hostname, so `uname -n` agrees on both sides.
    let hostname = recipe.as_ref().and_then(|r| r.hostname.clone());
    let mut executors = BTreeMap::new();
    for (exec_name, config) in &profile.executors {
      let key = config.key.render(&[
        ("session", &id),
        ("group", &group),
        ("profile", &profile_name),
        ("rule", exec_name),
      ]);
      let pool_key = format!("{exec_name}@{key}");
      let spec = InstanceSpec {
        name: exec_name.clone(),
        key: pool_key,
        session: id.clone(),
        config: config.clone(),
        base: base.clone(),
        user: profile.user.clone(),
        hostname: hostname.clone(),
      };
      let instance = self.instance(spec).await.with_context(|| format!("executor {exec_name}"))?;
      executors.insert(exec_name.clone(), instance);
    }
    let actor_command = if request.actor_command.is_empty() {
      profile.actor.command.to_vec()
    } else {
      request.actor_command.clone()
    };
    let injector = Arc::new(Injector::new(&profile.chaos));
    let session = Arc::new(Session {
      id: id.clone(),
      name: name.clone(),
      group: group.clone(),
      profile_name: profile_name.clone(),
      profile,
      created_ms: now_ms(),
      dir: dir.clone(),
      executors,
      state: Mutex::new(SessionState::Running),
      actor_command,
      recipe,
      injector,
      trace,
    });
    self.sessions.lock().unwrap().insert(id.clone(), session.clone());
    session.emit(&Event::SessionCreate(SessionCreate {
      name,
      group,
      profile: profile_name,
      executors: session
        .executors
        .iter()
        .map(|(n, i)| event::ExecutorRef {
          name: n.clone(),
          runtime: i.handle.runtime.into(),
          instance: i.handle.id.clone(),
        })
        .collect(),
    }));
    self.copy_in_executors(&session).await?;
    if request.start_actor {
      self.start_actor(&session).await?;
    }
    Ok(session)
  }

  /// Returns a pooled instance for the spec's key, creating it through the
  /// executor's provider when needed.
  async fn instance(&self, spec: InstanceSpec) -> Result<Arc<Instance>> {
    // TODO(8e): the pool lock is held while the provider creates the
    // instance; slice 8e fixes that with a regression test.
    let mut pools = self.pools.lock().await;
    if let Some(existing) = pools.get(&spec.key) {
      existing.refs.fetch_add(1, Ordering::SeqCst);
      return Ok(existing.clone());
    }
    let provider: Arc<dyn Provider> = match spec.config.runtime {
      Runtime::Native => self.native.clone(),
      Runtime::Remote => self.remote.clone(),
      Runtime::Docker => self.docker.clone().context("Docker is not available on this machine")?,
      // TODO(provider): containerd, firecracker and k8s providers are
      // deferred; see `needy_core_executor::provider`.
      other => bail!("runtime {other:?} is not supported yet"),
    };
    let handle = provider.create(&spec).await?;
    let instance = Arc::new(Instance {
      name: spec.name.clone(),
      handle,
      provider,
      path_map: spec.config.path_map.clone(),
      slots: spec.config.max_procs.map(|n| Arc::new(Semaphore::new(n as usize))),
      on_full: spec.config.on_full,
      refs: AtomicUsize::new(1),
    });
    pools.insert(spec.key, instance.clone());
    Ok(instance)
  }

  async fn release(&self, instance: &Arc<Instance>) {
    if instance.refs.fetch_sub(1, Ordering::SeqCst) > 1 {
      return;
    }
    let mut pools = self.pools.lock().await;
    pools.retain(|_, v| !Arc::ptr_eq(v, instance));
    if let Err(e) = instance.provider.destroy(&instance.handle).await {
      tracing::warn!("executor {}: cannot destroy {}: {e}", instance.name, instance.handle.id);
    }
  }

  async fn stop_session(&self, session: &Arc<Session>) {
    // TODO(8b): this does not stop the VM: its handle
    // (`needy_core_actor::host::vm::Vm`) is owned by the task that waits for
    // it in `start_actor`; slice 8b stops it here through `Vm::stop`, with a
    // regression test.
    if let Err(e) = self.mirror_out_executors(session).await {
      tracing::warn!("mirror-out for {}: {e:#}", session.id);
    }
    *session.state.lock().unwrap() = SessionState::Stopped;
    for instance in session.executors.values() {
      self.release(instance).await;
    }
    session.emit(&Event::SessionStop(SessionStop {}));
    // `needy report` right after the stop must see it, and the timing
    // summary is written now.
    session.trace.flush().await;
    self.sessions.lock().unwrap().remove(&session.id);
    self.ended.lock().unwrap().push(info(session));
  }

  // --------------------------------------------------------------- actor

  async fn start_actor(self: &Arc<Self>, session: &Arc<Session>) -> Result<()> {
    let image = vm::Image::new(&self.actor_image);
    image.check()?;
    let launch = vm::Launch::new(&image, &session.dir, session.recipe.as_ref());
    let api = Arc::new(Api {
      daemon: self.clone(),
    });
    let health = Health {
      instance_id: uuid::Uuid::new_v4().to_string(),
      role: "control",
    };
    let caller = Caller::Actor {
      session: session.id.clone(),
    };
    let guest_listener = Server::new(health, caller)
      .sessions(api.clone())
      .processes(api.clone())
      .guest(api)
      .bind(&[Listen::Unix(vm::host_port(&session.dir, GUEST_PORT))])
      .await?;
    let api_session = session.id.clone();
    // TODO(8c): the guest listener is not stopped with its actor; slice 8c
    // cleans up listener tasks and sockets.
    tokio::spawn(async move {
      if let Err(e) = guest_listener.serve(CancellationToken::new()).await {
        tracing::error!("session {api_session}: guest listener: {e:#}");
      }
    });
    // The actor owns its gateway: the gateway stops when the VM exits.
    let actor = CancellationToken::new();
    let gateway_path = vm::host_port(&session.dir, GATEWAY_PORT);
    let network = &session.profile.actor.network;
    let mut inject = BTreeMap::new();
    for (host, injection) in &network.inject {
      // The daemon holds the credential; the actor never sees it.
      match std::env::var(&injection.secret) {
        Ok(value) => {
          inject.insert(host.clone(), (injection.header.clone(), value));
        }
        Err(e) => tracing::warn!("session {}: no {} header for {host}: {}: {e}", session.id, injection.header, injection.secret),
      }
    }
    let policy = gateway::Policy {
      allow: network.allow.iter().map(|glob| glob.as_str().to_string()).collect(),
      upstream: network.upstream.clone(),
      inject,
    };
    let gateway =
      Arc::new(gateway::Gateway::new(policy, session.trace.clone(), Network::Actor, Some(session.injector.clone()), actor.clone()));
    // A socket left by an earlier daemon would make bind fail.
    if let Err(e) = std::fs::remove_file(&gateway_path)
      && e.kind() != std::io::ErrorKind::NotFound
    {
      tracing::warn!("cannot remove {}: {e}", gateway_path.display());
    }
    let listener = tokio::net::UnixListener::bind(&gateway_path).with_context(|| format!("cannot listen on {}", gateway_path.display()))?;
    // TODO(daemon): the gateway's own task joins the session's task tracker
    // when slice 8 gives sessions one.
    tokio::spawn(gateway.run(tokio_stream::wrappers::UnixListenerStream::new(listener)));

    let mut vm = match vm::Vm::start(&launch) {
      Ok(vm) => vm,
      Err(e) => {
        actor.cancel();
        return Err(e.into());
      }
    };
    session.emit(&Event::ActorStart(ActorStart {
      pid: vm.pid(),
      vcpus: launch.vcpus,
      mem_mib: launch.mem_mib,
    }));
    // Firecracker exits when the guest reboots after the driving program.
    let watch = session.clone();
    tokio::spawn(async move {
      let exit = match vm.wait().await {
        Ok(status) => {
          use std::os::unix::process::ExitStatusExt;
          ActorVmExit {
            code: status.code(),
            signal: status.signal().map(needy_core_executor::signals::name),
            error: None,
          }
        }
        Err(e) => ActorVmExit {
          code: None,
          signal: None,
          error: Some(e.to_string()),
        },
      };
      actor.cancel();
      watch.emit(&Event::ActorVmExit(exit));
      let mut state = watch.state.lock().unwrap();
      if *state == SessionState::Running {
        *state = SessionState::Finished;
      }
    });
    Ok(())
  }

  // ---------------------------------------------------------------- files

  fn host_path(session: &Session, path: &str) -> PathBuf {
    let path = path.replace("{session}", &session.id);
    let home = std::env::var("HOME").unwrap_or_default();
    let path = path.replacen('~', &home, 1);
    if Path::new(&path).is_absolute() {
      PathBuf::from(path)
    } else {
      session.profile.base_dir.join(path)
    }
  }

  /// Where the daemon's own `tar` runs: straight on the instance's
  /// executor, outside any session.
  fn files_target() -> FilesTarget {
    FilesTarget {
      executor: String::new(),
      session: String::new(),
      origin: WireOrigin::Files,
    }
  }

  async fn copy_in_executors(&self, session: &Arc<Session>) -> Result<()> {
    for (name, mapping) in &session.profile.files.mappings {
      if !session.profile.session_files.is_empty() && !session.profile.session_files.contains(name) {
        continue;
      }
      let (FileMode::CopyIn, Side::Host(src), Side::Executor { name: exec, path }) = (mapping.mode, &mapping.from, &mapping.to) else {
        continue;
      };
      let Some(instance) = session.executors.get(exec) else {
        continue;
      };
      let root = Self::host_path(session, src);
      let filter = mapping.filter.clone();
      let archive = tokio::task::spawn_blocking(move || needy_common::archive::pack(&root, &filter)).await??;
      let size = archive.len();
      files::push(&instance.handle.client, &Self::files_target(), archive, path).await.with_context(|| format!("copy-in {name}"))?;
      session.emit(&Event::FilesCopyIn(FilesCopyIn {
        name: name.clone(),
        to: FileEnd::Executor {
          name: exec.clone(),
          path: path.clone(),
        },
        bytes: size as u64,
      }));
    }
    Ok(())
  }

  async fn mirror_out_executors(&self, session: &Arc<Session>) -> Result<()> {
    for (name, mapping) in &session.profile.files.mappings {
      let (FileMode::MirrorOut, Side::Executor { name: exec, path }, Side::Host(dst)) = (mapping.mode, &mapping.from, &mapping.to) else {
        continue;
      };
      let Some(instance) = session.executors.get(exec) else {
        continue;
      };
      let archive =
        files::pull(&instance.handle.client, &Self::files_target(), path, ".").await.with_context(|| format!("mirror-out {name}"))?;
      let destination = Self::host_path(session, dst);
      let size = archive.len();
      tokio::task::spawn_blocking(move || needy_common::archive::unpack(&archive, &destination)).await??;
      session.emit(&Event::FilesMirrorOut(FilesMirrorOut {
        name: name.clone(),
        from: FileEnd::Executor {
          name: exec.clone(),
          path: path.clone(),
        },
        bytes: size as u64,
        entries: None,
      }));
    }
    Ok(())
  }

  // -------------------------------------------------------------- routing

  /// Runs the `exec.decide` chain for one command of `session`: rules, then
  /// chaos. A command placed on `executor` explicitly skips the rules. The
  /// delays of injected faults are applied here, and each fault is recorded.
  /// The chain's latency, without the delays, is the `exec.decide` timing.
  async fn decide(&self, session: &Session, ctx: &ExecContext, executor: Option<&str>) -> Decision {
    let started = std::time::Instant::now();
    let rules = Rules::new(&session.profile);
    let injector = session.injector.as_ref();
    let decision = match executor {
      None => intercept::decide(&[&rules, injector], ctx, Decision::unchanged(ctx, Placement::Actor)),
      Some(name) => intercept::decide(&[injector], ctx, Decision::unchanged(ctx, Placement::Executor(name.into()))),
    };
    session.trace.time("exec.decide", started.elapsed());
    for fired in &decision.fired {
      if let Some(delay) = fired.effect.delay {
        tokio::time::sleep(delay).await;
      }
      session.emit(&Event::FaultInjected(fired.clone()));
    }
    decision
  }

  /// Records an `exec.decide` event and returns it; `outcome` is where the
  /// command really runs, which for a command from the CLI may differ from
  /// the rules'.
  fn record_decision(&self, session: &Session, ctx: &ExecContext, decision: &Decision, origin: Origin, outcome: Outcome) -> Event {
    let event = Event::ExecDecide(ExecDecide {
      argv: ctx.argv.clone(),
      effective: decision.effective.clone(),
      origin,
      outcome,
      rules: decision.matched.clone(),
    });
    session.emit(&event);
    event
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

/// The command a decision runs on `instance`: its arguments and working
/// directory through the executor's path map, with the executable resolved
/// by the executor instead of the actor's path.
fn executor_command(instance: Option<&Instance>, decision: &Decision) -> (Vec<String>, String) {
  let mut argv = decision.argv.clone();
  let mut cwd = decision.cwd.clone();
  if let Some(instance) = instance {
    for (from, to) in &instance.path_map {
      cwd = map_prefix(&cwd, from, to);
      argv = argv.iter().map(|a| map_prefix(a, from, to)).collect();
    }
    if let Some(first) = argv.first_mut()
      && first.starts_with('/')
    {
      *first = needy_core::pattern::basename(first).to_string();
    }
  }
  (argv, cwd)
}

/// The variables a decision sets or changes relative to `asked`; all of them
/// when the decision clears the environment.
fn changed_env(decision: &Decision, asked: &BTreeMap<String, String>) -> HashMap<String, String> {
  decision.env.iter().filter(|(k, v)| asked.get(*k) != Some(*v) || decision.clear_env).map(|(k, v)| (k.clone(), v.clone())).collect()
}

fn exec_context(spec: &ExecSpec) -> ExecContext {
  ExecContext {
    argv: spec.argv.clone(),
    cwd: spec.cwd.clone(),
    env: spec.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
    tty: spec.tty.is_some(),
    // `Decide` fills the ancestry it carries; other execs have none.
    ancestors: Vec::new(),
  }
}

fn map_prefix(value: &str, from: &str, to: &str) -> String {
  if value == from {
    return to.to_string();
  }
  match value.strip_prefix(&format!("{}/", from.trim_end_matches('/'))) {
    Some(rest) => format!("{}/{rest}", to.trim_end_matches('/')),
    None => value.to_string(),
  }
}

fn info(session: &Session) -> SessionInfo {
  SessionInfo {
    id: session.id.clone(),
    name: session.name.clone(),
    group: session.group.clone(),
    state: *session.state.lock().unwrap(),
    executors: session
      .executors
      .iter()
      .map(|(n, i)| event::ExecutorRef {
        name: n.clone(),
        runtime: i.handle.runtime.into(),
        instance: i.handle.id.clone(),
      })
      .collect(),
    created_ms: session.created_ms,
    profile: session.profile_name.clone(),
  }
}

// ------------------------------------------------------------------ API

/// The daemon as `needy_api_server::control` sees it. Which session an
/// actor's call acts on is the listener's `Caller`, applied by the adapters.
struct Api {
  daemon: Arc<Daemon>,
}

impl Api {
  fn session(&self, id: &str) -> Result<Arc<Session>, ControlError> {
    self.daemon.session(id).ok_or_else(|| ControlError::NotFound("no such session".into()))
  }

  /// Places an exec of `session`: runs the `exec.decide` chain unless the
  /// command comes from the actor (the proxy decided already; a mount is not
  /// a command), rewrites `spec` and returns the instance it runs on with
  /// the events of the decision. A refusal carries those events.
  async fn place(&self, session: &Arc<Session>, spec: &mut ExecSpec) -> Result<(Arc<Instance>, Vec<Event>), ControlError> {
    let origin = spec.origin;
    let explicit = (!spec.executor.is_empty()).then(|| spec.executor.clone());
    let decision = match origin {
      Origin::ActorProxy | Origin::ActorMount => None,
      Origin::Cli | Origin::Fs | Origin::Files => {
        let ctx = exec_context(spec);
        Some((self.daemon.decide(session, &ctx, explicit.as_deref()).await, ctx))
      }
    };
    let mut events: Vec<Event> = decision.iter().flat_map(|(d, _)| d.fired.iter().cloned().map(Event::FaultInjected)).collect();
    if let Some((decision, ctx)) = &decision {
      let refusal = match &decision.outcome {
        Outcome::Deny(message) => Some(format!("denied: {message}")),
        Outcome::Fault(errno) => Some(format!("fault injected: {errno}")),
        Outcome::Run(_) => None,
      };
      if let Some(message) = refusal {
        if explicit.is_none() {
          events.push(self.daemon.record_decision(session, ctx, decision, origin, decision.outcome.clone()));
        }
        return Err(ControlError::Denied { message, events });
      }
    }
    if explicit.is_none() {
      // A command the rules keep on the actor runs on the default executor:
      // a CLI command has no actor to run on.
      spec.executor = match decision.as_ref().map(|(d, _)| &d.outcome) {
        Some(Outcome::Run(Placement::Executor(name))) => name.clone(),
        _ => session.profile.default_executor.clone().unwrap_or_default(),
      };
    }
    let instance = session
      .executors
      .get(&spec.executor)
      .or_else(|| session.executors.values().next())
      .cloned()
      .ok_or_else(|| ControlError::NotFound("the session has no executor".into()))?;
    if let (None, Some((decision, ctx))) = (&explicit, &decision) {
      let (argv, cwd) = executor_command(Some(&instance), decision);
      spec.argv = argv;
      spec.cwd = cwd;
      spec.env.extend(changed_env(decision, &ctx.env));
      // The decision records where the command really runs.
      events.push(self.daemon.record_decision(session, ctx, decision, origin, Outcome::Run(Placement::Executor(instance.name.clone()))));
    }
    Ok((instance, events))
  }
}

#[async_trait::async_trait]
impl Sessions for Api {
  async fn create(&self, request: NewSession) -> Result<SessionInfo, ControlError> {
    let session = self.daemon.create_session(request).await.map_err(failed)?;
    Ok(info(&session))
  }

  async fn get(&self, id: &str) -> Result<SessionInfo, ControlError> {
    if let Some(session) = self.daemon.session(id) {
      return Ok(info(&session));
    }
    let ended = self.daemon.ended.lock().unwrap();
    ended.iter().find(|s| s.id == id).cloned().ok_or_else(|| ControlError::NotFound("no such session".into()))
  }

  async fn list(&self) -> Result<Vec<SessionInfo>, ControlError> {
    let mut sessions: Vec<_> = self.daemon.sessions.lock().unwrap().values().map(|s| info(s)).collect();
    sessions.extend(self.daemon.ended.lock().unwrap().iter().cloned());
    sessions.sort_by_key(|s| s.created_ms);
    Ok(sessions)
  }

  async fn stop(&self, id: &str) -> Result<(), ControlError> {
    let session = self.session(id)?;
    self.daemon.stop_session(&session).await;
    Ok(())
  }

  async fn logs(&self, id: &str) -> Result<Vec<u8>, ControlError> {
    let dir = self.daemon.session_dir(id).ok_or_else(|| ControlError::NotFound("no such session".into()))?;
    // Firecracker writes the guest's serial console here.
    let path = vm::console(&dir);
    // TODO(session-logs): follow a running actor's console (`--follow`); not
    // needed by the QA item, which asks for the output so far.
    match tokio::fs::read(&path).await {
      Ok(bytes) => Ok(bytes),
      Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(ControlError::NotFound(format!("session {id} has no actor console"))),
      Err(e) => Err(ControlError::Internal(format!("cannot read {}: {e}", path.display()))),
    }
  }

  async fn decide(&self, session: &str, ctx: ExecContext) -> Result<Route, ControlError> {
    let session = self.session(session)?;
    // TODO(8f): no regression test covers an `under` rule matching a routed
    // command yet; slice 8f adds it and reruns one on the closed loop.
    let decision = self.daemon.decide(&session, &ctx, None).await;
    self.daemon.record_decision(&session, &ctx, &decision, Origin::ActorProxy, decision.outcome.clone());
    // A refusal is what the proxy prints, worded like `needy exec`'s.
    let (target, instance) = match &decision.outcome {
      Outcome::Run(Placement::Actor) => (Target::Actor, None),
      Outcome::Run(Placement::Executor(name)) => (Target::Executor(name.clone()), session.executors.get(name)),
      Outcome::Deny(message) => (Target::Refused(format!("denied: {message}")), None),
      Outcome::Fault(errno) => (Target::Refused(format!("fault injected: {errno}")), None),
    };
    let (argv, cwd) = executor_command(instance.map(Arc::as_ref), &decision);
    Ok(Route {
      target,
      argv,
      env: changed_env(&decision, &ctx.env),
      clear_env: decision.clear_env,
      cwd,
    })
  }
}

/// What the relay asks the instance for.
enum Upstream {
  Start(ExecSpec),
  Attach { process: String, replay: bool },
}

#[async_trait::async_trait]
impl Processes for Api {
  async fn exec(&self, open: ExecOpen, mut input: Inputs) -> Result<Outputs, ControlError> {
    let requested = std::time::Instant::now();
    let (instance, session, argv, origin, decided, mount_root, detach, open) = match open {
      ExecOpen::Start(mut spec) => {
        let origin = spec.origin;
        // The session comes from the start request, never from a lookup by
        // an empty ID, which as a prefix matches every session.
        let session = match spec.session.as_str() {
          "" => None,
          id => Some(self.session(id)?),
        };
        let (instance, decided) = match &session {
          Some(session) => self.place(session, &mut spec).await?,
          None => match spec.executor.as_str() {
            "" | "local" => (self.daemon.local.clone(), Vec::new()),
            name => self
              .daemon
              .pools
              .lock()
              .await
              .values()
              .find(|i| i.name == name)
              .cloned()
              .map(|instance| (instance, Vec::new()))
              .ok_or_else(|| ControlError::NotFound("no such executor".into()))?,
          },
        };
        if spec.cwd.is_empty() && !instance.handle.workspace.is_empty() {
          spec.cwd = instance.handle.workspace.clone();
        }
        spec.session = session.as_ref().map(|s| s.id.clone()).unwrap_or_default();
        // SFTP for an actor mount is framed so file faults can answer `open`.
        let mount_root = (origin == Origin::ActorMount).then(|| spec.cwd.clone());
        let (argv, detach) = (spec.argv.clone(), spec.detach);
        (instance, session, argv, Some(origin), decided, mount_root, detach, Upstream::Start(spec))
      }
      ExecOpen::Attach { process, replay } => {
        let instance =
          self.daemon.procs.lock().unwrap().get(&process).cloned().ok_or_else(|| ControlError::NotFound("no such process".into()))?;
        (instance, None, Vec::new(), None, Vec::new(), None, false, Upstream::Attach { process, replay })
      }
    };
    let permit = match &instance.slots {
      Some(slots) => Some(match instance.on_full {
        OnFull::Reject => slots.clone().try_acquire_owned().map_err(|_| ControlError::Exhausted("executor is full".into()))?,
        _ => slots.clone().acquire_owned().await.map_err(|_| ControlError::Unavailable("executor closed".into()))?,
      }),
      None => None,
    };
    // An exec without a session has no profile, so no chaos and no trace.
    let injector = session.as_ref().map(|s| s.injector.clone());
    let trace = session.as_ref().map(|s| s.trace.clone());
    let tap = match (&mount_root, &injector) {
      (Some(root), Some(injector)) if injector.has(FaultKind::FileOpen) => Some(Arc::new(SftpTap::new(root))),
      _ => None,
    };
    let (out_tx, out_rx) = mpsc::channel::<Result<ExecOutput, ControlError>>(64);
    for event in decided {
      out_tx.send(Ok(ExecOutput::Trace(event))).await.map_err(|_| ControlError::Internal("exec output channel closed".into()))?;
    }
    let client = &instance.handle.client;
    let mut upstream = match open {
      Upstream::Start(spec) => ExecStream::start(client, domain::proto_exec_start(spec)).await,
      Upstream::Attach { process, replay } => ExecStream::attach(client, &process, replay).await,
    }
    .map_err(relayed)?;
    {
      let upstream = upstream.input();
      let tap = tap.clone();
      let out_tx = out_tx.clone();
      let trace = trace.clone();
      tokio::spawn(async move {
        while let Some(mut message) = input.next().await {
          if let (Some(tap), Some(injector), ExecInput::Stdin(bytes)) = (&tap, &injector, &message) {
            let tapped = tap.client_bytes(bytes, injector);
            if let Some(trace) = &trace {
              for fired in tapped.injected {
                emit(trace, &Event::FaultInjected(fired));
              }
            }
            for reply in tapped.replies {
              // The client is gone; the input loop ends on its next read.
              if out_tx.send(Ok(ExecOutput::Stdout(reply))).await.is_err() {
                break;
              }
            }
            if tapped.forward.is_empty() {
              continue;
            }
            message = ExecInput::Stdin(tapped.forward);
          }
          let sent = match message {
            ExecInput::Stdin(bytes) => upstream.stdin(bytes).await,
            ExecInput::Eof => upstream.eof().await,
            ExecInput::Signal(name) => upstream.signal(&name).await,
            ExecInput::Resize(size) => upstream.resize(domain::proto_window(size)).await,
          };
          if sent.is_err() {
            break;
          }
        }
      });
    }
    let daemon = self.daemon.clone();
    let started_ms = now_ms();
    tokio::spawn(async move {
      let _permit = permit;
      let mut process_id = String::new();
      // A detached process keeps its slot until it exits, so the stream
      // is drained after the client leaves.
      let mut client_gone = false;
      loop {
        let output = match upstream.next().await {
          Ok(Some(event)) => event,
          Ok(None) => break,
          // The executor reported an error instead of an exit; the stream
          // goes on.
          Err(needy_api_client::Error::Remote(message)) => {
            if !client_gone && out_tx.send(Ok(ExecOutput::Error(message))).await.is_err() {
              if !detach {
                break;
              }
              client_gone = true;
            }
            continue;
          }
          Err(e) => {
            if out_tx.send(Err(relayed(e))).await.is_err() {
              tracing::debug!("exec relay: the client left before the upstream error");
            }
            break;
          }
        };
        let mut done = false;
        let output = match output {
          client_exec::Event::Started(started) => {
            process_id = started.process_id.clone();
            daemon.procs.lock().unwrap().insert(started.process_id.clone(), instance.clone());
            if let (Some(origin), Some(trace)) = (origin, &trace) {
              // From the request to the process running, placement and a
              // wait for the executor's slot included.
              trace.time("exec.start", requested.elapsed());
              emit(
                trace,
                &Event::ExecStart(ExecStart {
                  process: process_id.clone(),
                  executor: instance.name.clone(),
                  argv: argv.clone(),
                  origin,
                }),
              );
            }
            ExecOutput::Started(domain::started(started))
          }
          client_exec::Event::Stdout(mut bytes) => {
            if let Some(tap) = &tap {
              bytes = tap.server_bytes(&bytes);
              if bytes.is_empty() {
                continue;
              }
            }
            ExecOutput::Stdout(bytes)
          }
          client_exec::Event::Stderr(bytes) => ExecOutput::Stderr(bytes),
          client_exec::Event::Exited(exit) => {
            if let (Some(origin), Some(trace)) = (origin, &trace) {
              emit(
                trace,
                &Event::ExecExit(ExecExit {
                  process: process_id.clone(),
                  executor: instance.name.clone(),
                  argv: argv.clone(),
                  origin,
                  code: exit.code,
                  signal: (!exit.signal.is_empty()).then(|| exit.signal.clone()),
                  ms: (now_ms() - started_ms).max(0) as u64,
                }),
              );
            }
            done = true;
            ExecOutput::Exited(exit)
          }
          // Executors place nothing, so they send no trace events.
          client_exec::Event::Trace(_) => continue,
        };
        if done {
          if out_tx.send(Ok(output)).await.is_err() {
            tracing::debug!("exec relay: the client left before the exit");
          }
          break;
        }
        if !client_gone && out_tx.send(Ok(output)).await.is_err() {
          if !detach {
            break;
          }
          client_gone = true;
        }
      }
    });
    Ok(Box::pin(ReceiverStream::new(out_rx)))
  }

  async fn list(&self, session: &str) -> Result<Vec<ProcessInfo>, ControlError> {
    let mut instances: Vec<Arc<Instance>> = match self.daemon.session(session) {
      Some(session) => session.executors.values().cloned().collect(),
      None => {
        let mut all: Vec<_> = self.daemon.pools.lock().await.values().cloned().collect();
        all.push(self.daemon.local.clone());
        all
      }
    };
    instances.dedup_by(|a, b| Arc::ptr_eq(a, b));
    let request = ListProcessesRequest {
      session_id: session.to_string(),
    };
    let mut processes = Vec::new();
    for instance in instances {
      // An executor that does not answer has nothing to list.
      match instance.handle.client.processes().list_processes(request.clone()).await {
        Ok(list) => {
          for p in list.into_inner().processes {
            let mut p = domain::process_info(p);
            p.executor = instance.name.clone();
            self.daemon.procs.lock().unwrap().insert(p.process.clone(), instance.clone());
            processes.push(p);
          }
        }
        Err(status) => tracing::debug!("executor {}: cannot list processes: {}", instance.name, status.message()),
      }
    }
    processes.sort_by_key(|p| p.started_ms);
    Ok(processes)
  }

  async fn kill(&self, process: &str, signal: &str) -> Result<(), ControlError> {
    let instance =
      self.daemon.procs.lock().unwrap().get(process).cloned().ok_or_else(|| ControlError::NotFound("no such process".into()))?;
    let request = KillProcessRequest {
      process_id: process.to_string(),
      signal: signal.to_string(),
    };
    instance.handle.client.processes().kill_process(request).await.map_err(|s| relayed(s.into()))?;
    Ok(())
  }
}

#[async_trait::async_trait]
impl Guest for Api {
  async fn config(&self, session: &str) -> Result<GuestConfig, ControlError> {
    let session = self.session(session)?;
    let profile = &session.profile;
    let user = &profile.user;
    let home = format!("/home/{}", user.name);
    let mut mounts = Vec::new();
    for (name, mapping) in &profile.files.mappings {
      if !profile.session_files.is_empty() && !profile.session_files.contains(name) {
        continue;
      }
      if mapping.mode != FileMode::Mount {
        continue;
      }
      let (Side::Executor { name: exec, path }, Side::Actor(target)) = (&mapping.from, &mapping.to) else {
        continue;
      };
      let Some(instance) = session.executors.get(exec) else {
        continue;
      };
      mounts.push(GuestMount {
        target: target.clone(),
        executor: exec.clone(),
        source: path.clone(),
        writable: mapping.writable,
        sftp_command: instance.handle.sftp.clone(),
      });
    }
    let mut copy_in = Vec::new();
    let mut mirror_out = Vec::new();
    for (name, mapping) in &profile.files.mappings {
      if !profile.session_files.is_empty() && !profile.session_files.contains(name) {
        continue;
      }
      match (mapping.mode, &mapping.from, &mapping.to) {
        (FileMode::CopyIn, Side::Host(_), Side::Actor(path)) => copy_in.push(GuestCopy {
          name: name.clone(),
          path: path.clone(),
        }),
        (FileMode::MirrorOut, Side::Actor(path), Side::Host(_)) => mirror_out.push(GuestCopy {
          name: name.clone(),
          path: path.clone(),
        }),
        _ => {}
      }
    }
    let hwmon_faults = session
      .injector
      .faults()
      .iter()
      .filter(|f| f.on == FaultKind::Hwmon)
      .map(|f| GuestFault {
        at_ms: f.at.map(|d| d.as_millis() as i64).unwrap_or(0),
        temp_mc: f.set.temp_c.map(|c| (c * 1000.0) as i32).unwrap_or(100_000),
      })
      .collect();
    let recipe = session.recipe.as_ref();
    let routed = rules::route_names(profile);
    session.emit(&Event::ActorConfig(ActorConfig {
      route_names: routed.clone(),
      mounts: mounts.iter().map(|m| m.target.clone()).collect(),
    }));
    // `*` asks the kernel to route every exec.
    let route_names = routed.unwrap_or_else(|| vec!["*".into()]);
    Ok(GuestConfig {
      session: session.id.clone(),
      user: GuestUser {
        name: user.name.clone(),
        uid: user.uid,
        gid: user.gid,
      },
      route_names,
      hostname: recipe.and_then(|r| r.hostname.clone()).unwrap_or_default(),
      uptime_offset_secs: recipe.map(|r| r.uptime_offset_secs).unwrap_or(0),
      mounts,
      command: session.actor_command.clone(),
      env: profile.actor.env.clone().into_iter().collect(),
      cwd: profile.actor.cwd.clone().unwrap_or_else(|| format!("{home}/workspace")),
      gateway_port: GATEWAY_PORT,
      sensors: recipe
        .map(|r| {
          r.sensors
            .iter()
            .map(|s| GuestSensor {
              chip: s.chip.clone(),
              idle_mc: s.idle_mc,
            })
            .collect()
        })
        .unwrap_or_default(),
      copy_in,
      mirror_out,
      hwmon_faults,
    })
  }

  async fn event(&self, session: &str, time_ms: i64, event: Event) -> Result<(), ControlError> {
    let session = self.session(session)?;
    session.trace.emit_at(time_ms, event.kind().as_str(), event.raw());
    Ok(())
  }

  async fn exit(&self, session: &str, exit: Exit) -> Result<(), ControlError> {
    let session = self.session(session)?;
    session.emit(&Event::ActorExit(ActorExit {
      code: exit.code,
      signal: (!exit.signal.is_empty()).then_some(exit.signal),
    }));
    *session.state.lock().unwrap() = SessionState::Finished;
    Ok(())
  }

  async fn fetch_files(&self, session: &str, name: &str) -> Result<Vec<u8>, ControlError> {
    let session = self.session(session)?;
    let mapping = session
      .profile
      .files
      .mappings
      .get(name)
      .filter(|m| m.mode == FileMode::CopyIn)
      .ok_or_else(|| ControlError::NotFound("no such copy-in mapping".into()))?;
    let Side::Host(src) = &mapping.from else {
      return Err(ControlError::Invalid("copy-in must come from the host".into()));
    };
    let root = Daemon::host_path(&session, src);
    let filter = mapping.filter.clone();
    let archive = tokio::task::spawn_blocking(move || needy_common::archive::pack(&root, &filter))
      .await
      .map_err(|e| ControlError::Internal(e.to_string()))?
      .map_err(failed)?;
    session.emit(&Event::FilesCopyIn(FilesCopyIn {
      name: name.to_string(),
      to: FileEnd::Actor,
      bytes: archive.len() as u64,
    }));
    Ok(archive)
  }

  async fn push_files(&self, session: &str, name: &str, archive: Vec<u8>) -> Result<(), ControlError> {
    let session = self.session(session)?;
    let mapping = session
      .profile
      .files
      .mappings
      .get(name)
      .filter(|m| m.mode == FileMode::MirrorOut)
      .ok_or_else(|| ControlError::NotFound("no such mirror-out mapping".into()))?;
    let Side::Host(dst) = &mapping.to else {
      return Err(ControlError::Invalid("mirror-out must go to the host".into()));
    };
    let destination = Daemon::host_path(&session, dst);
    let size = archive.len();
    let count = tokio::task::spawn_blocking(move || needy_common::archive::unpack(&archive, &destination))
      .await
      .map_err(|e| ControlError::Internal(e.to_string()))?
      .map_err(failed)?;
    session.emit(&Event::FilesMirrorOut(FilesMirrorOut {
      name: name.to_string(),
      from: FileEnd::Actor,
      bytes: size as u64,
      entries: Some(count as u64),
    }));
    Ok(())
  }
}
