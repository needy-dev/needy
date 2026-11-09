//! The control daemon: sessions, executor instances, routing, the actor's
//! services and its gateway.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use needy_api_proto::v1::guest_service_server::{GuestService, GuestServiceServer};
use needy_api_proto::v1::health_service_server::HealthServiceServer;
use needy_api_proto::v1::process_service_server::{ProcessService, ProcessServiceServer};
use needy_api_proto::v1::session_service_server::{SessionService, SessionServiceServer};
use needy_api_proto::v1::{
  CreateSessionRequest, CreateSessionResponse, DecideRequest, DecideResponse, ExecRequest, ExecResponse, ExecutorRef, FetchFilesRequest,
  FetchFilesResponse, GetConfigRequest, GetConfigResponse, GetSessionLogsRequest, GetSessionLogsResponse, GetSessionRequest,
  GetSessionResponse, GuestCopy, GuestFault, GuestMount, GuestSensor, GuestUser, KillProcessRequest, KillProcessResponse,
  ListProcessesRequest, ListProcessesResponse, ListSessionsRequest, ListSessionsResponse, PushEventsRequest, PushEventsResponse,
  PushFilesRequest, PushFilesResponse, ReportExitRequest, ReportExitResponse, Session as SessionInfo, StopSessionRequest,
  StopSessionResponse, exec_request, exec_response,
};
use needy_api_server::protocol::domain;
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
use prost::Message as _;
use tokio::sync::{Semaphore, mpsc};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;
use tonic::{Request, Response, Status, Streaming};

use needy_core_actor::host::{gateway, vm};
use needy_core_executor::provider::{self, Docker, InstanceHandle, InstanceSpec, Native, Provider, Remote};
use needy_core_executor::service::ExecStream;
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

fn status(e: anyhow::Error) -> Status {
  Status::failed_precondition(format!("{e:#}"))
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
    let local_listen = format!("unix:{}", local_sock.display());
    let host = needy_core_executor::process::Host::default();
    let service = needy_core_executor::service::ExecutorService {
      host,
      name: "local".into(),
    };
    let routes = tonic::service::Routes::default()
      .add_service(HealthServiceServer::new(crate::server::Health {
        instance_id: uuid::Uuid::new_v4().to_string(),
        role: "executor",
      }))
      .add_service(ProcessServiceServer::new(service).max_decoding_message_size(usize::MAX).max_encoding_message_size(usize::MAX));
    tokio::spawn(async move {
      if let Err(e) = crate::server::serve(&local_listen, routes, None).await {
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

  pub fn routes(self: &Arc<Self>) -> tonic::service::Routes {
    tonic::service::Routes::default()
      .add_service(HealthServiceServer::new(crate::server::Health {
        instance_id: uuid::Uuid::new_v4().to_string(),
        role: "control",
      }))
      .add_service(SessionServiceServer::new(Api {
        daemon: self.clone(),
        bound: None,
      }))
      .add_service(
        ProcessServiceServer::new(Api {
          daemon: self.clone(),
          bound: None,
        })
        .max_decoding_message_size(usize::MAX)
        .max_encoding_message_size(usize::MAX),
      )
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
    let matches: Vec<_> = ended.iter().filter(|s| s.session_id.starts_with(id) || s.name == id).collect();
    (matches.len() == 1).then(|| self.state_dir.join("sessions").join(&matches[0].session_id))
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
  async fn create_session(self: &Arc<Self>, request: CreateSessionRequest) -> Result<Arc<Session>> {
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
          session_id: id.clone(),
          name: or_id(&request.name),
          group: or_id(&request.group),
          state: domain::proto_session_state(SessionState::Failed).into(),
          executors: Vec::new(),
          created_at_ms: created_ms,
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
    request: &CreateSessionRequest,
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
    self.ended.lock().unwrap().push(Api::info(session));
  }

  // --------------------------------------------------------------- actor

  async fn start_actor(self: &Arc<Self>, session: &Arc<Session>) -> Result<()> {
    let image = vm::Image::new(&self.actor_image);
    image.check()?;
    let launch = vm::Launch::new(&image, &session.dir, session.recipe.as_ref());
    let guest_listen = format!("unix:{}", vm::host_port(&session.dir, GUEST_PORT).display());
    let api = Api {
      daemon: self.clone(),
      bound: Some(session.id.clone()),
    };
    let api_session = session.id.clone();
    let routes = tonic::service::Routes::default()
      .add_service(GuestServiceServer::new(api.clone()))
      .add_service(SessionServiceServer::new(api.clone()))
      .add_service(ProcessServiceServer::new(api).max_decoding_message_size(usize::MAX).max_encoding_message_size(usize::MAX));
    // TODO(8c): the guest listener is not stopped with its actor; slice 8c
    // cleans up listener tasks and sockets.
    tokio::spawn(async move {
      if let Err(e) = crate::server::serve(&guest_listen, routes, None).await {
        tracing::error!("session {}: guest listener: {e:#}", api_session);
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

  /// Runs `argv` on an executor, feeding `input`, and returns stdout.
  async fn run_on(instance: &Instance, argv: Vec<String>, input: Option<Vec<u8>>) -> Result<Vec<u8>> {
    let (tx, rx) = mpsc::channel::<ExecRequest>(64);
    tx.send(ExecRequest {
      payload: Some(exec_request::Payload::Start(needy_api_proto::v1::ExecStart {
        argv,
        no_stdin: input.is_none(),
        env: [("COPYFILE_DISABLE".to_string(), "1".to_string())].into(),
        origin: domain::proto_origin(Origin::Files).into(),
        ..Default::default()
      })),
    })
    .await?;
    if let Some(bytes) = input {
      let tx = tx.clone();
      tokio::spawn(async move {
        // A closed channel means the command already exited; its exit
        // status, read below, reports why.
        for chunk in bytes.chunks(1 << 20) {
          let stdin = ExecRequest {
            payload: Some(exec_request::Payload::Stdin(chunk.to_vec())),
          };
          if tx.send(stdin).await.is_err() {
            return;
          }
        }
        let eof = ExecRequest {
          payload: Some(exec_request::Payload::StdinEof(true)),
        };
        if tx.send(eof).await.is_err() {
          tracing::debug!("the command exited before end of input");
        }
      });
    }
    let mut stream = instance.handle.client.processes().exec(ReceiverStream::new(rx)).await?.into_inner();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    while let Some(message) = stream.message().await? {
      match message.payload {
        Some(exec_response::Payload::Stdout(b)) => out.extend(b),
        Some(exec_response::Payload::Stderr(b)) => err.extend(b),
        Some(exec_response::Payload::Exited(exit)) => {
          if exit.code != Some(0) {
            bail!("{}", String::from_utf8_lossy(&err).trim());
          }
          break;
        }
        _ => {}
      }
    }
    drop(tx);
    Ok(out)
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
      Self::run_on(
        instance,
        vec![
          "tar".into(),
          "-x".into(),
          "-f".into(),
          "-".into(),
          "-C".into(),
          path.clone(),
        ],
        Some(archive),
      )
      .await
      .with_context(|| format!("copy-in {name}"))?;
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
      let archive = Self::run_on(
        instance,
        vec![
          "tar".into(),
          "-c".into(),
          "-f".into(),
          "-".into(),
          "-C".into(),
          path.clone(),
          ".".into(),
        ],
        None,
      )
      .await
      .with_context(|| format!("mirror-out {name}"))?;
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
fn load_profile(id: &str, request: &CreateSessionRequest) -> Result<(Profile, PathBuf)> {
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

/// Events in the form the exec stream carries them.
fn exec_events(events: &[Event]) -> Vec<needy_api_proto::v1::ExecEvent> {
  events
    .iter()
    .map(|event| needy_api_proto::v1::ExecEvent {
      kind: event.kind().as_str().into(),
      payload: event.payload(),
    })
    .collect()
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
fn changed_env(decision: &Decision, asked: &HashMap<String, String>) -> HashMap<String, String> {
  decision.env.iter().filter(|(k, v)| asked.get(*k) != Some(*v) || decision.clear_env).map(|(k, v)| (k.clone(), v.clone())).collect()
}

fn exec_context(argv: &[String], cwd: &str, env: &HashMap<String, String>, tty: bool) -> ExecContext {
  ExecContext {
    argv: argv.to_vec(),
    cwd: cwd.to_string(),
    env: env.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
    tty,
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

// ------------------------------------------------------------------ API

#[derive(Clone)]
struct Api {
  daemon: Arc<Daemon>,
  /// Session of an actor-facing listener.
  bound: Option<String>,
}

impl Api {
  /// Places an exec of `session`: runs the `exec.decide` chain unless the
  /// command comes from the actor (the proxy decided already; a mount is not
  /// a command), rewrites `start` and returns the instance it runs on with
  /// the events of the decision. A refusal's status carries those events as
  /// an `ExecRefusal` in its details.
  async fn place(
    &self,
    session: &Arc<Session>,
    start: &mut needy_api_proto::v1::ExecStart,
    origin: Origin,
  ) -> Result<(Arc<Instance>, Vec<Event>), Status> {
    let explicit = (!start.executor.is_empty()).then(|| start.executor.clone());
    let decision = match origin {
      Origin::ActorProxy | Origin::ActorMount => None,
      Origin::Cli | Origin::Fs | Origin::Files => {
        let ctx = exec_context(&start.argv, &start.cwd, &start.env, start.tty.is_some());
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
      if let Some(refusal) = refusal {
        if explicit.is_none() {
          events.push(self.daemon.record_decision(session, ctx, decision, origin, decision.outcome.clone()));
        }
        let details = needy_api_proto::v1::ExecRefusal {
          events: exec_events(&events),
        };
        return Err(Status::with_details(tonic::Code::PermissionDenied, refusal, details.encode_to_vec().into()));
      }
    }
    if explicit.is_none() {
      // A command the rules keep on the actor runs on the default executor:
      // a CLI command has no actor to run on.
      start.executor = match decision.as_ref().map(|(d, _)| &d.outcome) {
        Some(Outcome::Run(Placement::Executor(name))) => name.clone(),
        _ => session.profile.default_executor.clone().unwrap_or_default(),
      };
    }
    let instance = session
      .executors
      .get(&start.executor)
      .or_else(|| session.executors.values().next())
      .cloned()
      .ok_or_else(|| Status::not_found("the session has no executor"))?;
    if let (None, Some((decision, ctx))) = (&explicit, &decision) {
      let (argv, cwd) = executor_command(Some(&instance), decision);
      start.argv = argv;
      start.cwd = cwd;
      start.env.extend(changed_env(decision, &start.env));
      // The decision records where the command really runs.
      events.push(self.daemon.record_decision(session, ctx, decision, origin, Outcome::Run(Placement::Executor(instance.name.clone()))));
    }
    Ok((instance, events))
  }

  fn bound_session(&self) -> Result<Arc<Session>, Status> {
    let id = self.bound.as_ref().ok_or_else(|| Status::permission_denied("not an actor listener"))?;
    self.daemon.session(id).ok_or_else(|| Status::not_found("session ended"))
  }

  fn info(session: &Session) -> SessionInfo {
    SessionInfo {
      session_id: session.id.clone(),
      name: session.name.clone(),
      group: session.group.clone(),
      state: domain::proto_session_state(*session.state.lock().unwrap()).into(),
      executors: session
        .executors
        .iter()
        .map(|(n, i)| ExecutorRef {
          name: n.clone(),
          runtime: i.handle.runtime.into(),
          instance: i.handle.id.clone(),
        })
        .collect(),
      created_at_ms: session.created_ms,
      profile: session.profile_name.clone(),
    }
  }
}

#[tonic::async_trait]
impl SessionService for Api {
  async fn create_session(&self, request: Request<CreateSessionRequest>) -> Result<Response<CreateSessionResponse>, Status> {
    if self.bound.is_some() {
      return Err(Status::permission_denied("actors cannot create sessions"));
    }
    let request = request.into_inner();
    if request.profile_name.is_empty() {
      return Err(Status::invalid_argument("the request names no profile"));
    }
    let session = self.daemon.create_session(request).await.map_err(status)?;
    Ok(Response::new(CreateSessionResponse {
      session: Some(Self::info(&session)),
    }))
  }

  async fn get_session(&self, request: Request<GetSessionRequest>) -> Result<Response<GetSessionResponse>, Status> {
    let id = request.into_inner().session_id;
    if let Some(session) = self.daemon.session(&id) {
      return Ok(Response::new(GetSessionResponse {
        session: Some(Self::info(&session)),
      }));
    }
    let ended = self.daemon.ended.lock().unwrap();
    let session = ended.iter().find(|s| s.session_id == id).cloned().ok_or_else(|| Status::not_found("no such session"))?;
    Ok(Response::new(GetSessionResponse {
      session: Some(session),
    }))
  }

  async fn list_sessions(&self, _: Request<ListSessionsRequest>) -> Result<Response<ListSessionsResponse>, Status> {
    let mut sessions: Vec<_> = self.daemon.sessions.lock().unwrap().values().map(|s| Self::info(s)).collect();
    sessions.extend(self.daemon.ended.lock().unwrap().iter().cloned());
    sessions.sort_by_key(|s| s.created_at_ms);
    Ok(Response::new(ListSessionsResponse { sessions }))
  }

  type GetSessionLogsStream = std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<GetSessionLogsResponse, Status>> + Send>>;

  async fn get_session_logs(&self, request: Request<GetSessionLogsRequest>) -> Result<Response<Self::GetSessionLogsStream>, Status> {
    if self.bound.is_some() {
      return Err(Status::permission_denied("actors cannot read session logs"));
    }
    let id = request.into_inner().session_id;
    let dir = self.daemon.session_dir(&id).ok_or_else(|| Status::not_found("no such session"))?;
    // Firecracker writes the guest's serial console here.
    let path = vm::console(&dir);
    let console = match tokio::fs::read(&path).await {
      Ok(bytes) => bytes,
      Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
        return Err(Status::not_found(format!("session {id} has no actor console")));
      }
      Err(e) => return Err(Status::internal(format!("cannot read {}: {e}", path.display()))),
    };
    // TODO(session-logs): follow a running actor's console (`--follow`); not
    // needed by the QA item, which asks for the output so far.
    let chunks: Vec<Result<GetSessionLogsResponse, Status>> =
      console.chunks(1 << 20).map(|c| Ok(GetSessionLogsResponse { data: c.to_vec() })).collect();
    Ok(Response::new(Box::pin(tokio_stream::iter(chunks))))
  }

  async fn stop_session(&self, request: Request<StopSessionRequest>) -> Result<Response<StopSessionResponse>, Status> {
    if self.bound.is_some() {
      return Err(Status::permission_denied("actors cannot stop sessions"));
    }
    let session = self.daemon.session(&request.into_inner().session_id).ok_or_else(|| Status::not_found("no such session"))?;
    self.daemon.stop_session(&session).await;
    Ok(Response::new(StopSessionResponse {}))
  }

  async fn decide(&self, request: Request<DecideRequest>) -> Result<Response<DecideResponse>, Status> {
    let request = request.into_inner();
    let session = match &self.bound {
      Some(_) => self.bound_session()?,
      None => self.daemon.session(&request.session_id).ok_or_else(|| Status::not_found("no such session"))?,
    };
    let mut ctx = exec_context(&request.argv, &request.cwd, &request.env, request.tty);
    // TODO(8f): no regression test covers an `under` rule matching a routed
    // command yet; slice 8f adds it and reruns one on the closed loop.
    ctx.ancestors = request.ancestors.iter().map(|a| a.argv.clone()).collect();
    let decision = self.daemon.decide(&session, &ctx, None).await;
    self.daemon.record_decision(&session, &ctx, &decision, Origin::ActorProxy, decision.outcome.clone());
    // `deny` is what the proxy prints, worded like `needy exec`'s refusals.
    let (target, instance, deny) = match &decision.outcome {
      Outcome::Run(Placement::Actor) => ("actor".to_string(), None, String::new()),
      Outcome::Run(Placement::Executor(name)) => (name.clone(), session.executors.get(name), String::new()),
      Outcome::Deny(message) => (String::new(), None, format!("denied: {message}")),
      Outcome::Fault(errno) => (String::new(), None, format!("fault injected: {errno}")),
    };
    let (argv, cwd) = executor_command(instance.map(Arc::as_ref), &decision);
    Ok(Response::new(DecideResponse {
      target,
      argv,
      env: changed_env(&decision, &request.env),
      clear_env: decision.clear_env,
      cwd,
      deny,
    }))
  }
}

#[tonic::async_trait]
impl ProcessService for Api {
  type ExecStream = ExecStream;

  async fn exec(&self, request: Request<Streaming<ExecRequest>>) -> Result<Response<Self::ExecStream>, Status> {
    let requested = std::time::Instant::now();
    let mut inbound = request.into_inner();
    let mut first = inbound.message().await?.ok_or_else(|| Status::invalid_argument("empty request stream"))?;
    let (instance, session, argv, origin, decided) = match &mut first.payload {
      Some(exec_request::Payload::Start(start)) => {
        let origin = domain::origin(start.origin).map_err(|e| Status::invalid_argument(e.to_string()))?;
        let session = match &self.bound {
          Some(_) => Some(self.bound_session()?),
          None if !start.session_id.is_empty() => {
            Some(self.daemon.session(&start.session_id).ok_or_else(|| Status::not_found("no such session"))?)
          }
          None => None,
        };
        let (instance, decided) = match &session {
          Some(session) => self.place(session, start, origin).await?,
          None => match start.executor.as_str() {
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
              .ok_or_else(|| Status::not_found("no such executor"))?,
          },
        };
        if start.cwd.is_empty() && !instance.handle.workspace.is_empty() {
          start.cwd = instance.handle.workspace.clone();
        }
        start.session_id = session.as_ref().map(|s| s.id.clone()).unwrap_or_default();
        (instance, session, start.argv.clone(), Some(origin), decided)
      }
      Some(exec_request::Payload::Attach(attach)) => {
        let instance =
          self.daemon.procs.lock().unwrap().get(&attach.process_id).cloned().ok_or_else(|| Status::not_found("no such process"))?;
        (instance, None, Vec::new(), None, Vec::new())
      }
      _ => {
        return Err(Status::invalid_argument("the first request must start or attach"));
      }
    };
    let permit = match &instance.slots {
      Some(slots) => Some(match instance.on_full {
        OnFull::Reject => slots.clone().try_acquire_owned().map_err(|_| Status::resource_exhausted("executor is full"))?,
        _ => slots.clone().acquire_owned().await.map_err(|_| Status::unavailable("executor closed"))?,
      }),
      None => None,
    };
    let (mount_root, detach) = match &first.payload {
      Some(exec_request::Payload::Start(start)) => {
        let mount = origin == Some(Origin::ActorMount);
        (mount.then(|| start.cwd.clone()), start.detach)
      }
      _ => (None, false),
    };
    // The session comes from the start request, never from a lookup by an
    // empty ID, which as a prefix matches every session. An exec without a
    // session has no profile, so no chaos and no trace.
    let injector = session.as_ref().map(|s| s.injector.clone());
    let trace = session.as_ref().map(|s| s.trace.clone());
    // SFTP for an actor mount is framed so file faults can answer `open`.
    let tap = match (&mount_root, &injector) {
      (Some(root), Some(injector)) if injector.has(FaultKind::FileOpen) => Some(Arc::new(SftpTap::new(root))),
      _ => None,
    };
    let (out_tx, out_rx) = mpsc::channel::<Result<ExecResponse, Status>>(64);
    for event in exec_events(&decided) {
      let event = Ok(ExecResponse {
        payload: Some(exec_response::Payload::Event(event)),
      });
      out_tx.send(event).await.map_err(|_| Status::internal("channel"))?;
    }
    let (tx, rx) = mpsc::channel::<ExecRequest>(64);
    tx.send(first).await.map_err(|_| Status::internal("channel"))?;
    {
      let tap = tap.clone();
      let injector = injector.clone();
      let out_tx = out_tx.clone();
      let trace = trace.clone();
      tokio::spawn(async move {
        while let Ok(Some(mut message)) = inbound.message().await {
          if let (Some(tap), Some(injector), Some(exec_request::Payload::Stdin(bytes))) = (&tap, &injector, &message.payload) {
            let tapped = tap.client_bytes(bytes, injector);
            if let Some(trace) = &trace {
              for fired in tapped.injected {
                emit(trace, &Event::FaultInjected(fired));
              }
            }
            for reply in tapped.replies {
              let reply = Ok(ExecResponse {
                payload: Some(exec_response::Payload::Stdout(reply)),
              });
              // The client is gone; the inbound loop ends on its next read.
              if out_tx.send(reply).await.is_err() {
                break;
              }
            }
            if tapped.forward.is_empty() {
              continue;
            }
            message.payload = Some(exec_request::Payload::Stdin(tapped.forward));
          }
          if tx.send(message).await.is_err() {
            break;
          }
        }
      });
    }
    let mut upstream = instance.handle.client.processes().exec(ReceiverStream::new(rx)).await?.into_inner();
    let daemon = self.daemon.clone();
    let started_ms = now_ms();
    tokio::spawn(async move {
      let _permit = permit;
      let mut process_id = String::new();
      // A detached process keeps its slot until it exits, so the stream
      // is drained after the client leaves.
      let mut client_gone = false;
      loop {
        let mut message = match upstream.message().await {
          Ok(Some(m)) => m,
          Ok(None) => break,
          Err(e) => {
            if out_tx.send(Err(e)).await.is_err() {
              tracing::debug!("exec relay: the client left before the upstream error");
            }
            break;
          }
        };
        let mut done = false;
        match &mut message.payload {
          Some(exec_response::Payload::Started(started)) => {
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
          }
          Some(exec_response::Payload::Stdout(bytes)) => {
            if let Some(tap) = &tap {
              *bytes = tap.server_bytes(bytes);
              if bytes.is_empty() {
                continue;
              }
            }
          }
          Some(exec_response::Payload::Exited(exit)) => {
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
          }
          _ => {}
        }
        if done {
          if out_tx.send(Ok(message)).await.is_err() {
            tracing::debug!("exec relay: the client left before the exit");
          }
          break;
        }
        if !client_gone && out_tx.send(Ok(message)).await.is_err() {
          if !detach {
            break;
          }
          client_gone = true;
        }
      }
    });
    Ok(Response::new(Box::pin(ReceiverStream::new(out_rx))))
  }

  async fn list_processes(&self, request: Request<ListProcessesRequest>) -> Result<Response<ListProcessesResponse>, Status> {
    let mut request = request.into_inner();
    if let Some(id) = &self.bound {
      request.session_id = id.clone();
    }
    let mut instances: Vec<Arc<Instance>> = match self.daemon.session(&request.session_id) {
      Some(session) => session.executors.values().cloned().collect(),
      None => {
        let mut all: Vec<_> = self.daemon.pools.lock().await.values().cloned().collect();
        all.push(self.daemon.local.clone());
        all
      }
    };
    instances.dedup_by(|a, b| Arc::ptr_eq(a, b));
    let mut processes = Vec::new();
    for instance in instances {
      if let Ok(list) = instance.handle.client.processes().list_processes(request.clone()).await {
        for mut p in list.into_inner().processes {
          p.executor = instance.name.clone();
          self.daemon.procs.lock().unwrap().insert(p.process_id.clone(), instance.clone());
          processes.push(p);
        }
      }
    }
    processes.sort_by_key(|p| p.started_at_ms);
    Ok(Response::new(ListProcessesResponse { processes }))
  }

  async fn kill_process(&self, request: Request<KillProcessRequest>) -> Result<Response<KillProcessResponse>, Status> {
    let request = request.into_inner();
    let instance =
      self.daemon.procs.lock().unwrap().get(&request.process_id).cloned().ok_or_else(|| Status::not_found("no such process"))?;
    instance.handle.client.processes().kill_process(request).await
  }
}

#[tonic::async_trait]
impl GuestService for Api {
  async fn get_config(&self, _: Request<GetConfigRequest>) -> Result<Response<GetConfigResponse>, Status> {
    let session = self.bound_session()?;
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
    Ok(Response::new(GetConfigResponse {
      session_id: session.id.clone(),
      user: Some(GuestUser {
        name: user.name.clone(),
        uid: user.uid,
        gid: user.gid,
      }),
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
    }))
  }

  type FetchFilesStream = std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<FetchFilesResponse, Status>> + Send>>;

  async fn fetch_files(&self, request: Request<FetchFilesRequest>) -> Result<Response<Self::FetchFilesStream>, Status> {
    let session = self.bound_session()?;
    let name = request.into_inner().name;
    let mapping = session
      .profile
      .files
      .mappings
      .get(&name)
      .filter(|m| m.mode == FileMode::CopyIn)
      .ok_or_else(|| Status::not_found("no such copy-in mapping"))?;
    let Side::Host(src) = &mapping.from else {
      return Err(Status::invalid_argument("copy-in must come from the host"));
    };
    let root = Daemon::host_path(&session, src);
    let filter = mapping.filter.clone();
    let archive = tokio::task::spawn_blocking(move || needy_common::archive::pack(&root, &filter))
      .await
      .map_err(|e| Status::internal(e.to_string()))?
      .map_err(status)?;
    session.emit(&Event::FilesCopyIn(FilesCopyIn {
      name,
      to: FileEnd::Actor,
      bytes: archive.len() as u64,
    }));
    let chunks: Vec<Result<FetchFilesResponse, Status>> =
      archive.chunks(1 << 20).map(|c| Ok(FetchFilesResponse { data: c.to_vec() })).collect();
    Ok(Response::new(Box::pin(tokio_stream::iter(chunks))))
  }

  async fn push_files(&self, request: Request<Streaming<PushFilesRequest>>) -> Result<Response<PushFilesResponse>, Status> {
    let session = self.bound_session()?;
    let mut stream = request.into_inner();
    let mut name = String::new();
    let mut archive = Vec::new();
    while let Some(message) = stream.message().await? {
      if name.is_empty() {
        name = message.name;
      }
      archive.extend(message.data);
    }
    let mapping = session
      .profile
      .files
      .mappings
      .get(&name)
      .filter(|m| m.mode == FileMode::MirrorOut)
      .ok_or_else(|| Status::not_found("no such mirror-out mapping"))?;
    let Side::Host(dst) = &mapping.to else {
      return Err(Status::invalid_argument("mirror-out must go to the host"));
    };
    let destination = Daemon::host_path(&session, dst);
    let size = archive.len();
    let count = tokio::task::spawn_blocking(move || needy_common::archive::unpack(&archive, &destination))
      .await
      .map_err(|e| Status::internal(e.to_string()))?
      .map_err(status)?;
    session.emit(&Event::FilesMirrorOut(FilesMirrorOut {
      name,
      from: FileEnd::Actor,
      bytes: size as u64,
      entries: Some(count as u64),
    }));
    Ok(Response::new(PushFilesResponse {}))
  }

  async fn push_events(&self, request: Request<Streaming<PushEventsRequest>>) -> Result<Response<PushEventsResponse>, Status> {
    let session = self.bound_session()?;
    let mut stream = request.into_inner();
    while let Some(message) = stream.message().await? {
      if let Some(pushed) = message.event {
        let event = Event::decode(&pushed.kind, pushed.version, &pushed.payload)
          .map_err(|e| Status::invalid_argument(format!("guest event {}: {e}", pushed.kind)))?;
        session.trace.emit_at(pushed.time_ms, event.kind().as_str(), event.raw());
      }
    }
    Ok(Response::new(PushEventsResponse {}))
  }

  async fn report_exit(&self, request: Request<ReportExitRequest>) -> Result<Response<ReportExitResponse>, Status> {
    let session = self.bound_session()?;
    let exit = request.into_inner();
    session.emit(&Event::ActorExit(ActorExit {
      code: exit.code,
      signal: (!exit.signal.is_empty()).then_some(exit.signal),
    }));
    *session.state.lock().unwrap() = SessionState::Finished;
    Ok(Response::new(ReportExitResponse {}))
  }
}
