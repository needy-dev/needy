//! The control daemon: sessions, executor instances, routing, the actor's
//! services and its gateway.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use needy_api_client::Client;
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
use needy_config::Source;
use needy_config::profile::{Executor, FaultKind, FileMode, OnFull, Profile, Runtime, Side};
use needy_config::validate::{self, Severity};
use needy_core::recipe::{CompiledRecipe, HostInfo};
use needy_core::rules::{self, CommandContext, Target};
use tokio::sync::{Semaphore, mpsc};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming};

use needy_core::trace::{Trace, now_ms};
use needy_core_executor::service::ExecStream;

/// Ports the actor reaches on the host over vsock.
pub const GUEST_PORT: u32 = 7000;
pub const GATEWAY_PORT: u32 = 7001;

pub struct Instance {
  pub name: String,
  pub runtime: String,
  pub id: String,
  pub client: Client,
  pub sftp: Vec<String>,
  pub workspace: String,
  pub path_map: BTreeMap<String, String>,
  slots: Option<Arc<Semaphore>>,
  on_full: OnFull,
  refs: AtomicUsize,
  container: Option<String>,
}

struct InstanceRequest<'a> {
  key: &'a str,
  name: &'a str,
  config: &'a Executor,
  session: &'a str,
  profile: &'a Profile,
  base: &'a Path,
  hostname: Option<&'a str>,
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
  pub state: Mutex<String>,
  pub actor: Mutex<Option<tokio::process::Child>>,
  pub actor_command: Vec<String>,
  pub recipe: Option<CompiledRecipe>,
  pub injector: Arc<needy_core::chaos::Injector>,
}

pub struct Daemon {
  pub state_dir: PathBuf,
  pub actor_image: PathBuf,
  pub trace: Trace,
  sessions: Mutex<HashMap<String, Arc<Session>>>,
  /// Sessions that were stopped or failed to create, kept so `session ls`
  /// and `session logs` still find them.
  // TODO(persistence): this list lives in memory and grows until the daemon
  // exits; slice 9 keeps sessions in `state.db`.
  ended: Mutex<Vec<SessionInfo>>,
  pools: tokio::sync::Mutex<HashMap<String, Arc<Instance>>>,
  procs: Mutex<HashMap<String, Arc<Instance>>>,
  local: Arc<Instance>,
  docker_image: String,
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
    let trace = Trace::open(&state_dir.join("trace.db"), sessions_dir).await?;
    // This machine is always available as the `local` executor.
    let local_sock = runtime_dir.join("local.sock");
    let local_listen = format!("unix:{}", local_sock.display());
    let host = needy_core_executor::prochost::Host::default();
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
    let local_client = wait_connect(&format!("unix:{}", local_sock.display())).await?;
    let local = Arc::new(Instance {
      name: "local".into(),
      runtime: "native".into(),
      id: "local".into(),
      client: local_client,
      sftp: sftp_command_native(),
      workspace: String::new(),
      path_map: BTreeMap::new(),
      slots: None,
      on_full: OnFull::Wait,
      refs: AtomicUsize::new(1),
      container: None,
    });
    Ok(Arc::new(Self {
      state_dir,
      actor_image,
      trace,
      sessions: Mutex::new(HashMap::new()),
      ended: Mutex::new(Vec::new()),
      pools: tokio::sync::Mutex::new(HashMap::new()),
      procs: Mutex::new(HashMap::new()),
      local,
      docker_image,
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

  /// Creates a session; a failure is kept as a failed session.
  async fn create_session(self: &Arc<Self>, request: CreateSessionRequest) -> Result<Arc<Session>> {
    let id = uuid::Uuid::new_v4().simple().to_string()[..12].to_string();
    let created_ms = now_ms();
    match self.try_create_session(&id, &request).await {
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
          state: "failed".into(),
          executors: Vec::new(),
          created_at_ms: created_ms,
          profile: request.profile_dir.clone(),
        });
        Err(error)
      }
    }
  }

  async fn try_create_session(self: &Arc<Self>, id: &str, request: &CreateSessionRequest) -> Result<Arc<Session>> {
    let id = id.to_string();
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
    let profile_name = base.display().to_string();
    let recipe = match &profile.actor.recipe {
      Some(reference) => {
        let recipe = needy_config::recipe::load(&Source::reference(reference, &base))?;
        let host = HostInfo::detect().unwrap_or_else(|_| HostInfo::from_cpuinfo(""));
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
      let instance = self
        .instance(InstanceRequest {
          key: &pool_key,
          name: exec_name,
          config,
          session: &id,
          profile: &profile,
          base: &base,
          hostname: hostname.as_deref(),
        })
        .await
        .with_context(|| format!("executor {exec_name}"))?;
      executors.insert(exec_name.clone(), instance);
    }
    let actor_command = if request.actor_command.is_empty() {
      profile.actor.command.to_vec()
    } else {
      request.actor_command.clone()
    };
    let injector = Arc::new(needy_core::chaos::Injector::new(&profile.chaos));
    let session = Arc::new(Session {
      id: id.clone(),
      name: name.clone(),
      group: group.clone(),
      profile_name: profile_name.clone(),
      profile,
      created_ms: now_ms(),
      dir: dir.clone(),
      executors,
      state: Mutex::new("running".into()),
      actor: Mutex::new(None),
      actor_command,
      recipe,
      injector,
    });
    self.sessions.lock().unwrap().insert(id.clone(), session.clone());
    self.trace.record(
      &id,
      "session.create",
      [
        ("name", name),
        ("group", group),
        ("profile", profile_name),
        ("executors", session.executors.iter().map(|(n, i)| format!("{n}={}:{}", i.runtime, i.id)).collect::<Vec<_>>().join(",")),
      ],
    );
    self.copy_in_executors(&session).await?;
    if request.start_actor {
      self.start_actor(&session).await?;
    }
    Ok(session)
  }

  /// Returns a pooled instance for `key`, creating it when needed.
  async fn instance(&self, request: InstanceRequest<'_>) -> Result<Arc<Instance>> {
    let InstanceRequest {
      key,
      name,
      config,
      session,
      profile,
      base,
      hostname,
    } = request;
    let mut pools = self.pools.lock().await;
    if let Some(existing) = pools.get(key) {
      existing.refs.fetch_add(1, Ordering::SeqCst);
      return Ok(existing.clone());
    }
    let slots = config.max_procs.map(|n| Arc::new(Semaphore::new(n as usize)));
    let user = &profile.user;
    let instance = match config.runtime {
      Runtime::Native => Instance {
        name: name.into(),
        runtime: "native".into(),
        id: "local".into(),
        client: self.local.client.clone(),
        sftp: sftp_command_native(),
        workspace: config.workspace.clone().map(|w| expand(&w, base)).unwrap_or_default(),
        path_map: config.path_map.clone(),
        slots,
        on_full: config.on_full,
        refs: AtomicUsize::new(1),
        container: None,
      },
      Runtime::Remote => {
        let remote = config.remote.clone().context("remote executor needs `remote`")?;
        let mut client = wait_connect(&remote.url).await?;
        if let Some(env) = &remote.token_env {
          let token = std::env::var(env).with_context(|| format!("{env} is not set"))?;
          client = client.with_token(&token)?;
        }
        let platform =
          client.health().check(needy_api_proto::v1::CheckRequest {}).await.map(|r| r.into_inner().platform).unwrap_or_default();
        Instance {
          name: name.into(),
          runtime: "remote".into(),
          id: remote.url.clone(),
          client,
          sftp: sftp_command_for(&platform),
          workspace: config.workspace.clone().unwrap_or_default(),
          path_map: config.path_map.clone(),
          slots,
          on_full: config.on_full,
          refs: AtomicUsize::new(1),
          container: None,
        }
      }
      Runtime::Docker => {
        let docker = config.docker.clone().unwrap_or_default();
        if let Err(e) = needy_core::docker::check_docker_args(&docker.args, &[]) {
          bail!("{e}");
        }
        let image = docker.image.clone().unwrap_or_else(|| self.docker_image.clone());
        let container = format!("needy-{}", sanitize(key));
        let sock_dir = self.state_dir.join("executors").join(sanitize(key));
        std::fs::create_dir_all(&sock_dir)?;
        let host_ws = match &config.workspace {
          Some(w) => PathBuf::from(expand(w, base)),
          None => self.state_dir.join("workspaces").join(sanitize(key)),
        };
        std::fs::create_dir_all(&host_ws)?;
        let home = format!("/home/{}", user.name);
        // A container left from an earlier daemon would block the name. When
        // there is none, `docker rm` exits non-zero, which is the expected
        // case; only failing to run docker at all is worth a log line.
        if let Err(e) = tokio::process::Command::new("docker").args(["rm", "-f", &container]).output().await {
          tracing::debug!("docker rm {container}: {e}");
        }
        let mut args: Vec<String> = vec![
          "run".into(),
          "-d".into(),
          "--rm".into(),
          "--name".into(),
          container.clone(),
          "--label".into(),
          format!("needy.session={session}"),
          "--user".into(),
          format!("{}:{}", user.uid, user.gid),
          "-v".into(),
          format!("{}:{home}/workspace", host_ws.display()),
          "-v".into(),
          format!("{}:/run/needy", sock_dir.display()),
        ];
        if let Some(hostname) = hostname {
          args.extend(["--hostname".into(), hostname.to_string()]);
        }
        args.extend(docker.args.iter().cloned());
        args.extend([
          image,
          "needy".into(),
          "daemon".into(),
          "--mode".into(),
          "executor".into(),
          "--listen".into(),
          "unix:/run/needy/executor.sock".into(),
          "--name".into(),
          name.into(),
        ]);
        let output = tokio::process::Command::new("docker").args(&args).output().await?;
        if !output.status.success() {
          bail!("docker run failed: {}", String::from_utf8_lossy(&output.stderr));
        }
        let client = wait_connect(&format!("unix:{}", sock_dir.join("executor.sock").display())).await?;
        Instance {
          name: name.into(),
          runtime: "docker".into(),
          id: container.clone(),
          client,
          sftp: vec!["/usr/lib/openssh/sftp-server".into()],
          workspace: format!("{home}/workspace"),
          path_map: config.path_map.clone(),
          slots,
          on_full: config.on_full,
          refs: AtomicUsize::new(1),
          container: Some(container),
        }
      }
      ref other => bail!("runtime {other:?} is not supported yet"),
    };
    let instance = Arc::new(instance);
    pools.insert(key.to_string(), instance.clone());
    Ok(instance)
  }

  async fn release(&self, instance: &Arc<Instance>) {
    if instance.refs.fetch_sub(1, Ordering::SeqCst) > 1 {
      return;
    }
    let mut pools = self.pools.lock().await;
    pools.retain(|_, v| !Arc::ptr_eq(v, instance));
    if let Some(container) = &instance.container {
      match tokio::process::Command::new("docker").args(["rm", "-f", container]).output().await {
        Ok(output) if !output.status.success() => {
          tracing::warn!("docker rm {container}: {}", String::from_utf8_lossy(&output.stderr).trim());
        }
        Err(e) => tracing::warn!("docker rm {container}: {e}"),
        Ok(_) => {}
      }
    }
  }

  async fn stop_session(&self, session: &Arc<Session>) {
    // TODO(8b): `actor` is never set, so this does not stop the VM; slice 8b
    // fixes that with a regression test.
    if let Some(mut child) = session.actor.lock().unwrap().take()
      && let Err(e) = child.start_kill()
    {
      tracing::warn!("session {}: cannot kill the actor: {e}", session.id);
    }
    if let Err(e) = self.mirror_out_executors(session).await {
      tracing::warn!("mirror-out for {}: {e:#}", session.id);
    }
    *session.state.lock().unwrap() = "stopped".into();
    for instance in session.executors.values() {
      self.release(instance).await;
    }
    self.trace.record(&session.id, "session.stop", [("", "")]);
    self.sessions.lock().unwrap().remove(&session.id);
    self.ended.lock().unwrap().push(Api::info(session));
  }

  // --------------------------------------------------------------- actor

  async fn start_actor(self: &Arc<Self>, session: &Arc<Session>) -> Result<()> {
    let image = &self.actor_image;
    let vmlinux = image.join("vmlinux");
    let rootfs = image.join("rootfs.ext4");
    let firecracker = image.join("firecracker");
    for path in [&vmlinux, &rootfs, &firecracker] {
      if !path.exists() {
        bail!("actor image is missing {}; run `needy build`", path.display());
      }
    }
    let vsock = session.dir.join("v.sock");
    // Guest-initiated vsock connections land on `<uds>_<port>`.
    let guest_listen = format!("unix:{}_{GUEST_PORT}", vsock.display());
    let api = Api {
      daemon: self.clone(),
      bound: Some(session.id.clone()),
    };
    let api_session = session.id.clone();
    let routes = tonic::service::Routes::default()
      .add_service(GuestServiceServer::new(api.clone()))
      .add_service(SessionServiceServer::new(api.clone()))
      .add_service(ProcessServiceServer::new(api).max_decoding_message_size(usize::MAX).max_encoding_message_size(usize::MAX));
    tokio::spawn(async move {
      if let Err(e) = crate::server::serve(&guest_listen, routes, None).await {
        tracing::error!("session {}: guest listener: {e:#}", api_session);
      }
    });
    let gateway_path = format!("{}_{GATEWAY_PORT}", vsock.display());
    let network = &session.profile.actor.network;
    let policy = needy_core_actor::gateway::Policy {
      allow: network.allow.iter().map(|glob| glob.as_str().to_string()).collect(),
      upstream: network.upstream.clone(),
      inject: network.inject.iter().map(|(h, i)| (h.clone(), (i.header.clone(), i.secret.clone()))).collect(),
    };
    let gateway = Arc::new(needy_core_actor::gateway::Gateway::new(
      policy,
      self.trace.clone(),
      session.id.clone(),
      "actor",
      Some(session.injector.clone()),
    ));
    // A socket left by an earlier daemon would make bind fail.
    if let Err(e) = std::fs::remove_file(&gateway_path)
      && e.kind() != std::io::ErrorKind::NotFound
    {
      tracing::warn!("cannot remove {gateway_path}: {e}");
    }
    let listener = tokio::net::UnixListener::bind(&gateway_path)?;
    tokio::spawn(async move {
      while let Ok((stream, _)) = listener.accept().await {
        tokio::spawn(gateway.clone().serve(stream));
      }
    });

    let recipe = session.recipe.clone();
    let (vcpus, mem) = recipe.as_ref().map(|r| (r.vcpus.max(1), r.mem_mib.max(256))).unwrap_or((2, 2048));
    let pci = recipe.as_ref().is_some_and(|r| r.firecracker_env.keys().any(|k| k.starts_with("NEEDY_PCI")));
    let mut boot_args = "console=ttyS0 reboot=k panic=1 root=/dev/vda ro init=/.needy/init quiet loglevel=4".to_string();
    if !pci {
      boot_args.push_str(" pci=off");
    }
    let mut config = serde_json::json!({
        "boot-source": {"kernel_image_path": vmlinux, "boot_args": boot_args},
        "drives": [{"drive_id": "rootfs", "path_on_host": rootfs, "is_root_device": true, "is_read_only": true}],
        "machine-config": {"vcpu_count": vcpus, "mem_size_mib": mem},
        "vsock": {"guest_cid": 3, "uds_path": vsock},
    });
    if let Some(r) = &recipe
      && r.cpu_template.get("cpuid_modifiers").and_then(|m| m.as_array()).is_some_and(|m| !m.is_empty())
    {
      config["cpu-config"] = r.cpu_template.clone();
    }
    let config_path = session.dir.join("vm.json");
    std::fs::write(&config_path, serde_json::to_vec_pretty(&config)?)?;
    let console = std::fs::File::create(session.dir.join("console.log"))?;
    let mut command = tokio::process::Command::new(&firecracker);
    command
      .args(["--no-api", "--config-file"])
      .arg(&config_path)
      .stdin(std::process::Stdio::null())
      .stdout(console.try_clone()?)
      .stderr(console);
    if pci {
      command.arg("--enable-pci");
    }
    if let Some(r) = &recipe {
      command.envs(&r.firecracker_env);
    }
    let mut child = command.spawn().context("cannot start firecracker")?;
    let pid = child.id().unwrap_or(0);
    self.trace.record(
      &session.id,
      "actor.start",
      [
        ("pid", pid.to_string()),
        ("vcpus", vcpus.to_string()),
        ("mem_mib", mem.to_string()),
      ],
    );
    // Firecracker exits when the guest reboots after the driving program.
    let daemon = self.clone();
    let id = session.id.clone();
    let watch = session.clone();
    tokio::spawn(async move {
      let status = child.wait().await;
      daemon.trace.record(&id, "actor.vm.exit", [("status", format!("{status:?}"))]);
      let mut state = watch.state.lock().unwrap();
      if *state == "running" {
        *state = "finished".into();
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
        origin: "files".into(),
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
    let mut stream = instance.client.processes().exec(ReceiverStream::new(rx)).await?.into_inner();
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
      self.trace.record(
        &session.id,
        "files.copy_in",
        [
          ("name", name.clone()),
          ("to", format!("{exec}:{path}")),
          ("bytes", size.to_string()),
        ],
      );
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
      self.trace.record(
        &session.id,
        "files.mirror_out",
        [
          ("name", name.clone()),
          ("from", format!("{exec}:{path}")),
          ("bytes", size.to_string()),
        ],
      );
    }
    Ok(())
  }

  // ---------------------------------------------------------------- chaos

  /// Applies `exec` faults: delays sleep here; an errno denies the command.
  async fn exec_fault(&self, session: &Session, command: &str) -> Option<String> {
    let fired = session.injector.check(FaultKind::Exec, command)?;
    if let Some(delay) = fired.delay {
      tokio::time::sleep(delay).await;
    }
    self.trace.record(
      &session.id,
      "fault.injected",
      [
        ("on", "exec".to_string()),
        ("argv", command.to_string()),
        ("errno", fired.errno.clone().unwrap_or_default()),
        ("delay_ms", fired.delay.map(|d| d.as_millis().to_string()).unwrap_or_default()),
      ],
    );
    fired.errno
  }

  // -------------------------------------------------------------- routing

  fn decide(&self, session: &Session, request: &DecideRequest) -> DecideResponse {
    let ctx = CommandContext {
      argv: request.argv.clone(),
      cwd: request.cwd.clone(),
      env: request.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
      tty: request.tty,
      ancestors: Vec::new(),
    };
    let decision = rules::decide(&session.profile, &ctx);
    let (target, deny) = match &decision.target {
      Target::Actor => ("actor".to_string(), String::new()),
      Target::Executor(name) => (name.clone(), String::new()),
      Target::Deny(message) => (String::new(), message.clone()),
    };
    let mut argv = decision.argv.clone();
    let mut cwd = decision.cwd.clone();
    if let Some(instance) = session.executors.get(&target) {
      for (from, to) in &instance.path_map {
        cwd = map_prefix(&cwd, from, to);
        argv = argv.iter().map(|a| map_prefix(a, from, to)).collect();
      }
      // Run the executable the executor resolves, not the actor's path.
      if let Some(first) = argv.first_mut()
        && first.starts_with('/')
      {
        *first = Path::new(first.as_str()).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| first.clone());
      }
    }
    let env: HashMap<String, String> = decision
      .env
      .iter()
      .filter(|(k, v)| request.env.get(*k) != Some(*v) || decision.clear_env)
      .map(|(k, v)| (k.clone(), v.clone()))
      .collect();
    self.trace.record(
      &session.id,
      "exec.decide",
      [
        ("argv", request.argv.join(" ")),
        ("effective", decision.effective.join(" ")),
        (
          "target",
          if deny.is_empty() {
            target.clone()
          } else {
            "deny".into()
          },
        ),
        ("deny", deny.clone()),
        ("rules", decision.matched.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",")),
      ],
    );
    DecideResponse {
      target,
      argv,
      env,
      clear_env: decision.clear_env,
      cwd,
      deny,
    }
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

fn expand(path: &str, base: &Path) -> String {
  let home = std::env::var("HOME").unwrap_or_default();
  let path = path.replacen('~', &home, 1);
  if Path::new(&path).is_absolute() {
    path
  } else {
    base.join(path).display().to_string()
  }
}

fn sanitize(key: &str) -> String {
  key
    .chars()
    .map(|c| {
      if c.is_ascii_alphanumeric() || c == '-' {
        c
      } else {
        '-'
      }
    })
    .collect()
}

fn sftp_command_native() -> Vec<String> {
  sftp_command_for(&format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH))
}

fn sftp_command_for(platform: &str) -> Vec<String> {
  if platform.starts_with("macos") {
    vec!["/usr/libexec/sftp-server".into()]
  } else if platform.starts_with("windows") {
    vec![r"C:\Windows\System32\OpenSSH\sftp-server.exe".into()]
  } else {
    vec!["/usr/lib/openssh/sftp-server".into()]
  }
}

async fn wait_connect(endpoint: &str) -> Result<Client> {
  let mut last = None;
  for _ in 0..100 {
    match Client::connect(endpoint).await {
      Ok(client) => {
        // Any answer, even an error status, means the server is up.
        match client.health().check(needy_api_proto::v1::CheckRequest {}).await {
          Err(status) if status.code() == tonic::Code::Unavailable => {}
          _ => return Ok(client),
        }
      }
      Err(e) => last = Some(e),
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
  }
  bail!("cannot reach {endpoint}: {last:?}")
}

// ------------------------------------------------------------------ API

#[derive(Clone)]
struct Api {
  daemon: Arc<Daemon>,
  /// Session of an actor-facing listener.
  bound: Option<String>,
}

impl Api {
  fn bound_session(&self) -> Result<Arc<Session>, Status> {
    let id = self.bound.as_ref().ok_or_else(|| Status::permission_denied("not an actor listener"))?;
    self.daemon.session(id).ok_or_else(|| Status::not_found("session ended"))
  }

  fn info(session: &Session) -> SessionInfo {
    SessionInfo {
      session_id: session.id.clone(),
      name: session.name.clone(),
      group: session.group.clone(),
      state: session.state.lock().unwrap().clone(),
      executors: session
        .executors
        .iter()
        .map(|(n, i)| ExecutorRef {
          name: n.clone(),
          runtime: i.runtime.clone(),
          instance: i.id.clone(),
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
    let session = self.daemon.create_session(request.into_inner()).await.map_err(status)?;
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
    let path = dir.join("console.log");
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
    let mut response = self.daemon.decide(&session, &request);
    // `deny` is what the proxy prints, worded like `needy exec`'s refusals.
    if !response.deny.is_empty() {
      response.deny = format!("denied: {}", response.deny);
    } else if let Some(errno) = self.daemon.exec_fault(&session, &request.argv.join(" ")).await {
      response.target = String::new();
      response.deny = format!("fault injected: {errno}");
    }
    Ok(Response::new(response))
  }
}

#[tonic::async_trait]
impl ProcessService for Api {
  type ExecStream = ExecStream;

  async fn exec(&self, request: Request<Streaming<ExecRequest>>) -> Result<Response<Self::ExecStream>, Status> {
    let mut inbound = request.into_inner();
    let mut first = inbound.message().await?.ok_or_else(|| Status::invalid_argument("empty request stream"))?;
    let (instance, session_id, argv, origin) = match &mut first.payload {
      Some(exec_request::Payload::Start(start)) => {
        let session = match &self.bound {
          Some(_) => Some(self.bound_session()?),
          None if !start.session_id.is_empty() => {
            Some(self.daemon.session(&start.session_id).ok_or_else(|| Status::not_found("no such session"))?)
          }
          None => None,
        };
        let instance = match &session {
          Some(session) => {
            if start.executor.is_empty() {
              let decision = self.daemon.decide(
                session,
                &DecideRequest {
                  session_id: session.id.clone(),
                  argv: start.argv.clone(),
                  cwd: start.cwd.clone(),
                  env: start.env.clone(),
                  tty: start.tty.is_some(),
                },
              );
              if !decision.deny.is_empty() {
                return Err(Status::permission_denied(format!("denied: {}", decision.deny)));
              }
              let target = if decision.target == "actor" {
                session.profile.default_executor.clone().unwrap_or_default()
              } else {
                decision.target
              };
              start.executor = target;
              start.argv = decision.argv;
              start.cwd = decision.cwd;
              start.env.extend(decision.env);
            }
            session
              .executors
              .get(&start.executor)
              .or_else(|| session.executors.values().next())
              .cloned()
              .ok_or_else(|| Status::not_found("the session has no executor"))?
          }
          None => match start.executor.as_str() {
            "" | "local" => self.daemon.local.clone(),
            name => self
              .daemon
              .pools
              .lock()
              .await
              .values()
              .find(|i| i.name == name)
              .cloned()
              .ok_or_else(|| Status::not_found("no such executor"))?,
          },
        };
        if start.cwd.is_empty() && !instance.workspace.is_empty() {
          start.cwd = instance.workspace.clone();
        }
        let session_id = session.as_ref().map(|s| s.id.clone()).unwrap_or_default();
        start.session_id = session_id.clone();
        // Chaos and faults that act on commands fire before anything runs.
        if let Some(session) = &session
          && start.origin != "actor-proxy"
          && start.origin != "actor-mount"
          && let Some(errno) = self.daemon.exec_fault(session, &start.argv.join(" ")).await
        {
          return Err(Status::permission_denied(format!("fault injected: {errno}")));
        }
        (instance, session_id, start.argv.clone(), start.origin.clone())
      }
      Some(exec_request::Payload::Attach(attach)) => {
        let instance =
          self.daemon.procs.lock().unwrap().get(&attach.process_id).cloned().ok_or_else(|| Status::not_found("no such process"))?;
        (instance, String::new(), Vec::new(), "attach".into())
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
    let (tty, mount_root, detach) = match &first.payload {
      Some(exec_request::Payload::Start(start)) => {
        (start.tty.map(|t| (t.cols, t.rows)), (start.origin == "actor-mount").then(|| start.cwd.clone()), start.detach)
      }
      _ => (None, None, false),
    };
    let session = self.daemon.session(&session_id);
    let injector = session.as_ref().map(|s| s.injector.clone());
    // SFTP for an actor mount is framed so file faults can answer `open`.
    let tap = match (&mount_root, &injector) {
      (Some(root), Some(injector)) if injector.has(FaultKind::FileOpen) => Some(Arc::new(needy_core::chaos::SftpTap::new(root))),
      _ => None,
    };
    let (out_tx, out_rx) = mpsc::channel::<Result<ExecResponse, Status>>(64);
    let (tx, rx) = mpsc::channel::<ExecRequest>(64);
    tx.send(first).await.map_err(|_| Status::internal("channel"))?;
    {
      let tap = tap.clone();
      let injector = injector.clone();
      let out_tx = out_tx.clone();
      let trace = self.daemon.trace.clone();
      let session_id = session_id.clone();
      tokio::spawn(async move {
        while let Ok(Some(mut message)) = inbound.message().await {
          if let (Some(tap), Some(injector), Some(exec_request::Payload::Stdin(bytes))) = (&tap, &injector, &message.payload) {
            let tapped = tap.client_bytes(bytes, injector);
            for (path, errno) in &tapped.injected {
              trace.record(&session_id, "fault.injected", [("on", "file.open"), ("path", path), ("errno", errno)]);
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
    let mut upstream = instance.client.processes().exec(ReceiverStream::new(rx)).await?.into_inner();
    let daemon = self.daemon.clone();
    let started_ms = now_ms();
    let cast_dir = session.as_ref().map(|s| s.dir.join("casts"));
    tokio::spawn(async move {
      let _permit = permit;
      let mut process_id = String::new();
      let mut cast: Option<std::fs::File> = None;
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
            let mut cast_path = String::new();
            if let (Some((cols, rows)), Some(dir)) = (tty, &cast_dir) {
              cast = needy_core::trace::open_cast(dir, &process_id, cols, rows, &argv);
              if cast.is_some() {
                cast_path = dir.join(format!("{process_id}.cast")).display().to_string();
              }
            }
            if !argv.is_empty() {
              daemon.trace.record(
                &session_id,
                "exec.start",
                [
                  ("process", process_id.clone()),
                  ("executor", instance.name.clone()),
                  ("argv", argv.join(" ")),
                  ("origin", origin.clone()),
                  ("cast", cast_path),
                ],
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
            if let Some(file) = cast.as_mut() {
              needy_core::trace::cast_output(file, started_ms, bytes);
            }
          }
          Some(exec_response::Payload::Exited(exit)) => {
            if !argv.is_empty() {
              daemon.trace.record(
                &session_id,
                "exec.exit",
                [
                  ("process", process_id.clone()),
                  ("executor", instance.name.clone()),
                  ("argv", argv.join(" ")),
                  ("origin", origin.clone()),
                  ("code", exit.code.map(|c| c.to_string()).unwrap_or_default()),
                  ("signal", exit.signal.clone()),
                  ("ms", (now_ms() - started_ms).to_string()),
                ],
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
      if let Ok(list) = instance.client.processes().list_processes(request.clone()).await {
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
    instance.client.processes().kill_process(request).await
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
        sftp_command: instance.sftp.clone(),
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
    let route_names = rules::route_names(profile).unwrap_or_else(|| vec!["*".into()]);
    self.daemon.trace.record(
      &session.id,
      "actor.config",
      [
        ("route", route_names.join(",")),
        ("mounts", mounts.iter().map(|m| m.target.clone()).collect::<Vec<_>>().join(",")),
      ],
    );
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
    self.daemon.trace.record(
      &session.id,
      "files.copy_in",
      [
        ("name", name),
        ("to", "actor".into()),
        ("bytes", archive.len().to_string()),
      ],
    );
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
    self.daemon.trace.record(
      &session.id,
      "files.mirror_out",
      [
        ("name", name),
        ("from", "actor".into()),
        ("bytes", size.to_string()),
        ("entries", count.to_string()),
      ],
    );
    Ok(Response::new(PushFilesResponse {}))
  }

  async fn push_events(&self, request: Request<Streaming<PushEventsRequest>>) -> Result<Response<PushEventsResponse>, Status> {
    let session = self.bound_session()?;
    let mut stream = request.into_inner();
    while let Some(message) = stream.message().await? {
      if let Some(event) = message.event {
        self.daemon.trace.record_event(needy_core::trace::Event {
          session: session.id.clone(),
          time_ms: event.time_ms,
          kind: event.kind,
          attrs: event.attrs.into_iter().collect(),
        });
      }
    }
    Ok(Response::new(PushEventsResponse {}))
  }

  async fn report_exit(&self, request: Request<ReportExitRequest>) -> Result<Response<ReportExitResponse>, Status> {
    let session = self.bound_session()?;
    let exit = request.into_inner();
    self.daemon.trace.record(
      &session.id,
      "actor.exit",
      [
        ("code", exit.code.map(|c| c.to_string()).unwrap_or_default()),
        ("signal", exit.signal),
      ],
    );
    *session.state.lock().unwrap() = "finished".into();
    Ok(Response::new(ReportExitResponse {}))
  }
}
