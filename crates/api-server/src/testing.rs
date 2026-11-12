//! A fake daemon for the adapters' tests: it implements the domain traits,
//! records what it was asked, and answers predictably. Tests drive it
//! through a real [`Server`] and `needy-api-client`.

use std::collections::HashMap;
use std::sync::Mutex;

use needy_api_client::Client;
use needy_common::process::Exit;
use needy_config::profile::ExecutorDriver;
use needy_core::event::{Event, RunStop};
use needy_core::intercept::exec::ExecContext;
use needy_core::types::{Capabilities, Origin, Principal, RunState};
use tokio_stream::StreamExt as _;
use tokio_util::sync::CancellationToken;

use crate::control::*;
use crate::protocol::grpc::v1::health::Health;
use crate::server::{Listen, Server};
use needy_api_proto::v1;

#[derive(Default)]
pub struct Fake {
  /// What the fake was asked, one line per call.
  pub calls: Mutex<Vec<String>>,
  pub contexts: Mutex<Vec<ExecContext>>,
  pub events: Mutex<Vec<(String, i64, Event)>>,
  pub exits: Mutex<Vec<(String, Exit)>>,
  pub pushed: Mutex<Vec<(String, String, Vec<u8>)>>,
}

impl Fake {
  fn call(&self, line: String) {
    self.calls.lock().unwrap().push(line);
  }

  pub fn calls(&self) -> Vec<String> {
    self.calls.lock().unwrap().clone()
  }
}

/// The event a refused exec carries.
pub fn refusal_event() -> Event {
  Event::RunStop(RunStop::default())
}

fn info(id: &str, name: &str) -> RunInfo {
  RunInfo {
    id: id.into(),
    name: name.into(),
    group: id.into(),
    state: RunState::Running,
    executors: Vec::new(),
    released: Vec::new(),
    lost: Vec::new(),
    created_ms: 1,
    profile: "fake".into(),
    actor_exit: None,
    failure: None,
    tty: false,
    actor: None,
  }
}

#[async_trait::async_trait]
impl Runs for Fake {
  async fn create(&self, request: NewRun) -> Result<RunInfo> {
    match &request.by {
      Some(Principal::Operator) | None => self.call(format!("create {}", request.profile_name)),
      Some(by) => self.call(format!("create {} by {by}", request.profile_name)),
    }
    Ok(info("s1", &request.name))
  }

  async fn get(&self, id: &str) -> Result<RunInfo> {
    match id {
      "s1" => Ok(info("s1", "one")),
      _ => Err(ControlError::NotFound("no such run".into())),
    }
  }

  async fn list(&self) -> Result<Vec<RunInfo>> {
    Ok(vec![info("s1", "one"), info("s2", "two")])
  }

  async fn stop(&self, id: &str, by: Principal) -> Result<()> {
    self.call(format!("stop {id} by {by}"));
    Ok(())
  }

  /// Without `follow`, more than one message, so the adapter must chunk
  /// it; with it, two reads and the end of the run.
  async fn logs(&self, id: &str, follow: bool) -> Result<Logs> {
    self.call(format!("logs {id} follow={follow}"));
    let reads = if follow {
      vec![Ok(b"boot\n".to_vec()), Ok(b"later\n".to_vec())]
    } else {
      vec![Ok(vec![b'c'; 3 << 20])]
    };
    Ok(Box::pin(tokio_stream::iter(reads)))
  }

  /// Run `s1` has 2500 events, more than one message holds; `empty` has
  /// none.
  async fn events(&self, run: &str) -> Result<RunEvents> {
    self.call(format!("events {run}"));
    let count = match run {
      "s" | "s1" | "latest" => 2500,
      "empty" => 0,
      _ => return Err(ControlError::NotFound(format!("no run matches {run}"))),
    };
    let events = (0..count)
      .map(|seq| StoredEvent {
        seq,
        time_ms: 1000 + seq as i64,
        kind: "run.stop".into(),
        version: needy_core::event::SCHEMA_VERSION,
        payload: "{}".into(),
      })
      .collect();
    let run = if run == "empty" { "empty" } else { "s1" };
    Ok(RunEvents {
      run: run.into(),
      events,
    })
  }

  async fn decide(&self, run: &str, ctx: ExecContext) -> Result<Route> {
    self.call(format!("decide {run}"));
    self.contexts.lock().unwrap().push(ctx.clone());
    let target = match ctx.argv.first().map(String::as_str) {
      Some("rm") => Target::Refused("denied: no deleting".into()),
      Some("ls") => Target::Actor,
      _ => Target::Executor(format!("on-{run}")),
    };
    Ok(Route {
      target,
      argv: ctx.argv,
      env: HashMap::from([("SET".into(), "1".into())]),
      clear_env: false,
      cwd: ctx.cwd,
      search_path_if_missing: false,
    })
  }

  /// The actor on runner `kvm`, executor `mac` on runner `mac` and `build`
  /// on the control host, with one error finding.
  async fn check(&self, profile: ProfileToCheck) -> Result<ProfileCheck> {
    self.call(format!("check {}", profile.profile_yaml));
    Ok(ProfileCheck {
      actor: Some(Placement::Runner("kvm".into())),
      executors: [
        ("build".to_string(), Placement::ControlHost),
        ("mac".to_string(), Placement::Runner("mac".into())),
      ]
      .into(),
      actor_cpu: "Fake CPU".into(),
      actor_image: "needy-custom-jq".into(),
      actor_base_abi: Some(1),
      findings: vec![ProfileFinding {
        error: true,
        message: "recipe: avx512f is missing".into(),
      }],
    })
  }

  async fn add_fault(&self, run: &str, name: &str, fault_yaml: &str, by: Principal) -> Result<FaultAdded> {
    self.call(format!("add fault {name} to {run} by {by}: {fault_yaml}"));
    Ok(FaultAdded {
      run: "s1".into(),
      warnings: vec!["a warning".into()],
    })
  }

  async fn faults(&self, run: &str) -> Result<RunFaults> {
    self.call(format!("faults {run}"));
    Ok(RunFaults {
      run: "s1".into(),
      faults: vec![FaultInfo {
        name: "curl-down".into(),
        domain: "exec".into(),
        fault_yaml: "exec: curl **\nexit: 7\n".into(),
        source: needy_core::chaos::FaultSource::Added,
        matched: 2,
        fired: 1,
      }],
    })
  }

  async fn remove_fault(&self, run: &str, name: &str, by: Principal) -> Result<String> {
    self.call(format!("remove fault {name} from {run} by {by}"));
    Ok("s1".into())
  }
}

#[async_trait::async_trait]
impl Processes for Fake {
  /// Echoes the stdin of a start until end of input, then exits 3. The
  /// started process's executor names the run it was placed in.
  async fn exec(&self, open: ExecOpen, mut input: Inputs) -> Result<Outputs> {
    let (process, executor) = match open {
      ExecOpen::Start(spec) => {
        match &spec.by {
          Some(Principal::Operator) | None => self.call(format!("exec {} in {:?} from {:?}", spec.argv.join(" "), spec.run, spec.origin)),
          Some(by) => self.call(format!("exec {} in {:?} from {:?} by {by}", spec.argv.join(" "), spec.run, spec.origin)),
        }
        if spec.argv.first().map(String::as_str) == Some("refuse") {
          return Err(ControlError::Denied {
            message: "denied: refused".into(),
            events: vec![refusal_event()],
          });
        }
        if spec.argv.first().map(String::as_str) == Some("full") {
          return Err(ControlError::Exhausted("executor is full".into()));
        }
        ("p1".to_string(), spec.run)
      }
      ExecOpen::Attach {
        process, replay, ..
      } => {
        self.call(format!("attach {process} replay={replay}"));
        (process, String::new())
      }
    };
    let stream = async_stream(process, executor, async move {
      let mut echoed = Vec::new();
      while let Some(item) = input.next().await {
        match item {
          ExecInput::Stdin(bytes) => echoed.extend(bytes),
          ExecInput::Eof => break,
          ExecInput::Signal(_) | ExecInput::Resize(_) | ExecInput::Detach => {}
        }
      }
      echoed
    });
    Ok(stream)
  }

  async fn list(&self, run: &str, runner: &str) -> Result<Vec<ProcessInfo>> {
    if runner.is_empty() {
      self.call(format!("list {run:?}"));
    } else {
      self.call(format!("list {run:?} on runner {runner}"));
    }
    Ok(vec![ProcessInfo {
      process: "p1".into(),
      argv: vec!["sleep".into()],
      executor: "here".into(),
      pid: 42,
      running: false,
      exit: Some(Exit {
        code: None,
        signal: "SIGKILL".into(),
      }),
      tty: false,
      started_ms: 7,
      run: run.into(),
      origin: Some(Origin::Cli),
    }])
  }

  async fn kill(&self, process: &str, signal: &str, by: Principal) -> Result<()> {
    match by {
      Principal::Operator => self.call(format!("kill {process} {signal}")),
      by => self.call(format!("kill {process} {signal} by {by}")),
    }
    match process {
      "p1" => Ok(()),
      _ => Err(ControlError::NotFound("no such process".into())),
    }
  }
}

/// Started, the echoed bytes, then an exit with 3.
fn async_stream(process: String, executor: String, echo: impl Future<Output = Vec<u8>> + Send + 'static) -> Outputs {
  let (tx, rx) = tokio::sync::mpsc::channel(8);
  tokio::spawn(async move {
    let started = ExecOutput::Started(Started {
      process,
      executor,
      pid: 42,
      argv: Vec::new(),
      cwd: String::new(),
    });
    // The test reading the stream may have left; nothing waits for these.
    let _ = tx.send(Ok(started)).await;
    let echoed = echo.await;
    let _ = tx.send(Ok(ExecOutput::Stdout(echoed))).await;
    let exited = ExecOutput::Exited(Exit {
      code: Some(3),
      signal: String::new(),
    });
    let _ = tx.send(Ok(exited)).await;
  });
  Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx))
}

#[async_trait::async_trait]
impl Guest for Fake {
  async fn config(&self, run: &str) -> Result<GuestConfig> {
    Ok(GuestConfig {
      run: run.into(),
      user: GuestUser {
        name: "agent".into(),
        uid: 1000,
        gid: 1000,
      },
      route_names: vec!["*".into()],
      mounts: vec![GuestMount {
        target: "/home/agent/workspace".into(),
        executor: "here".into(),
        source: "/srv".into(),
        writable: true,
        sftp_command: vec!["sftp-server".into()],
      }],
      gateway_port: 7001,
      ..Default::default()
    })
  }

  async fn event(&self, run: &str, time_ms: i64, event: Event) -> Result<()> {
    self.events.lock().unwrap().push((run.into(), time_ms, event));
    Ok(())
  }

  async fn exit(&self, run: &str, exit: Exit) -> Result<()> {
    self.exits.lock().unwrap().push((run.into(), exit));
    Ok(())
  }

  async fn fetch_files(&self, _run: &str, name: &str) -> Result<Vec<u8>> {
    match name {
      "big" => Ok(vec![b'f'; 3 << 20]),
      _ => Err(ControlError::NotFound("no such copy-in mapping".into())),
    }
  }

  async fn push_files(&self, run: &str, name: &str, archive: Vec<u8>) -> Result<()> {
    self.pushed.lock().unwrap().push((run.into(), name.into(), archive));
    Ok(())
  }
}

fn fake_runner(name: &str) -> RunnerInfo {
  RunnerInfo {
    name: name.into(),
    capabilities: Capabilities {
      executor: [ExecutorDriver::Native].into(),
      ..Default::default()
    },
    labels: [("os".to_string(), "darwin".to_string())].into(),
    status: RunnerStatus::Online,
    created_ms: 1,
    last_seen_ms: 2,
    rotating: false,
    join_token: "t1".into(),
    host: None,
  }
}

/// Credentials the fake knows: `needy-runner.mac.good` is the runner
/// `mac`'s; `needy-runner.gone.old` was revoked.
pub const MAC_CREDENTIAL: &str = "needy-runner.mac.good";
pub const REVOKED_CREDENTIAL: &str = "needy-runner.gone.old";

#[async_trait::async_trait]
impl Runners for Fake {
  async fn create_join_token(&self, request: NewJoinToken) -> Result<CreatedJoinToken> {
    self.call(format!("create token ttl={} uses={} roles={:?} labels={:?}", request.ttl_ms, request.uses, request.roles, request.labels));
    Ok(CreatedJoinToken {
      info: JoinTokenInfo {
        id: "t1".into(),
        roles: request.roles,
        labels: request.labels,
        created_ms: 1,
        expires_ms: 1 + request.ttl_ms,
        uses_left: request.uses,
        state: JoinTokenState::Valid,
      },
      token: "needy-join.t1.secret".into(),
    })
  }

  async fn join_tokens(&self) -> Result<Vec<JoinTokenInfo>> {
    self.call("list tokens".into());
    Ok(Vec::new())
  }

  async fn revoke_join_token(&self, id: &str) -> Result<()> {
    self.call(format!("revoke {id}"));
    Ok(())
  }

  async fn join(&self, request: Joining) -> Result<Joined> {
    self.call(format!("join {} with {} as {}", request.name, request.join_token, request.report.capabilities));
    if request.join_token != "needy-join.t1.secret" {
      return Err(ControlError::Unauthenticated("the join token is not valid".into()));
    }
    Ok(Joined {
      runner: fake_runner(&request.name),
      credential: MAC_CREDENTIAL.into(),
      heartbeat_interval_ms: 15_000,
    })
  }

  async fn heartbeat(&self, runner: &str, report: RunnerReport) -> Result<Beat> {
    self.call(format!("heartbeat {runner} {}", report.capabilities));
    Ok(Beat {
      credential: Some("needy-runner.mac.new".into()),
      heartbeat_interval_ms: 15_000,
    })
  }

  async fn runners(&self) -> Result<Vec<RunnerInfo>> {
    self.call("list runners".into());
    Ok(vec![fake_runner("mac")])
  }

  async fn rotate(&self, runner: &str) -> Result<()> {
    self.call(format!("rotate {runner}"));
    Ok(())
  }

  async fn remove(&self, runner: &str) -> Result<()> {
    self.call(format!("remove {runner}"));
    Ok(())
  }

  async fn credential(&self, token: &str) -> Credential {
    match token {
      MAC_CREDENTIAL => Credential::Runner("mac".into()),
      REVOKED_CREDENTIAL => Credential::Refused,
      _ => Credential::Other,
    }
  }

  /// Announces one list task.
  async fn control(&self, runner: &str, _results: FromRunner<v1::ControlRequest>) -> Result<ToRunner<v1::ControlResponse>> {
    self.call(format!("control {runner}"));
    let task = v1::ControlResponse {
      payload: Some(v1::control_response::Payload::Task(v1::RunnerTask {
        id: 1,
        kind: Some(v1::runner_task::Kind::ListProcesses(v1::ListProcessesRequest::default())),
      })),
    };
    Ok(Box::pin(tokio_stream::iter([task])))
  }

  /// Sends one line of input for task 7 and refuses every other task.
  async fn task(&self, runner: &str, task: u64, _responses: FromRunner<v1::TaskRequest>) -> Result<ToRunner<v1::TaskResponse>> {
    self.call(format!("task {runner} {task}"));
    if task != 7 {
      return Err(ControlError::NotFound("no such task".into()));
    }
    let request = v1::TaskResponse {
      exec: Some(v1::ExecRequest {
        payload: Some(v1::exec_request::Payload::Stdin(b"hi".to_vec())),
      }),
    };
    Ok(Box::pin(tokio_stream::iter([request])))
  }
}

/// Tokens the fake's client registry knows: `CLIENT_TOKEN` is the client
/// `laptop`'s; `REVOKED_CLIENT_TOKEN` was revoked.
pub const CLIENT_TOKEN: &str = "needy-client.c1.good";
pub const REVOKED_CLIENT_TOKEN: &str = "needy-client.c2.old";
pub const PAIRING_CODE: &str = "needy-pair.k1.secret";

fn fake_client() -> ClientInfo {
  ClientInfo {
    id: "c1".into(),
    name: "laptop".into(),
    created_ms: 1,
    last_used_ms: None,
    revoked_ms: None,
  }
}

#[async_trait::async_trait]
impl Clients for Fake {
  async fn create_pairing_code(&self, request: NewPairingCode) -> Result<CreatedPairingCode> {
    self.call(format!("create pairing code ttl={} name={}", request.ttl_ms, request.name));
    // `laptop` is the fake's active client, so a code for its name
    // replaces it.
    let name = if request.name.is_empty() {
      "client-k1".to_string()
    } else {
      request.name
    };
    Ok(CreatedPairingCode {
      id: "k1".into(),
      replaces: (name == "laptop").then(fake_client),
      name,
      expires_ms: 1 + request.ttl_ms,
      code: PAIRING_CODE.into(),
    })
  }

  async fn pair(&self, code: &str) -> Result<Paired> {
    self.call(format!("pair {code}"));
    if code != PAIRING_CODE {
      return Err(ControlError::Unauthenticated("the pairing code is not valid".into()));
    }
    Ok(Paired {
      client: fake_client(),
      token: CLIENT_TOKEN.into(),
    })
  }

  async fn clients(&self) -> Result<Vec<ClientInfo>> {
    self.call("list clients".into());
    Ok(vec![fake_client()])
  }

  async fn revoke(&self, id: &str) -> Result<()> {
    self.call(format!("revoke client {id}"));
    Ok(())
  }

  async fn credential(&self, token: &str) -> Credential {
    match token {
      CLIENT_TOKEN => Credential::Client {
        id: "c1".into(),
        name: "laptop".into(),
      },
      REVOKED_CLIENT_TOKEN => Credential::Refused,
      _ => Credential::Other,
    }
  }
}

/// A server for `caller` with every service on the fake, on a socket in a
/// temporary directory, and a client of it. The server stops with the
/// returned token's guard.
pub struct Served {
  pub fake: std::sync::Arc<Fake>,
  pub client: Client,
  _dir: tempfile::TempDir,
  _stop: tokio_util::sync::DropGuard,
}

pub async fn serve(caller: Caller) -> Served {
  serve_with(caller, None).await
}

/// As [`serve`], with a bearer token. On Windows, which binds no Unix socket
/// (`Error::Unsupported`), the server listens on a loopback TCP port, as
/// [`serve_tcp`]; the services and their callers are the same.
pub async fn serve_with(caller: Caller, token: Option<&str>) -> Served {
  let dir = tempfile::tempdir().unwrap();
  #[cfg(unix)]
  let listen = Listen::Unix(dir.path().join("api.sock"));
  #[cfg(not(unix))]
  let listen = Listen::Tcp("127.0.0.1:0".into());
  serve_on(caller, token, listen, dir).await
}

/// As [`serve_with`], on a loopback TCP port instead of a Unix socket, as a
/// control plane reached through a tunnel.
pub async fn serve_tcp(caller: Caller, token: Option<&str>) -> Served {
  serve_on(caller, token, Listen::Tcp("127.0.0.1:0".into()), tempfile::tempdir().unwrap()).await
}

async fn serve_on(caller: Caller, token: Option<&str>, listen: Listen, dir: tempfile::TempDir) -> Served {
  let fake = std::sync::Arc::new(Fake::default());
  let health = Health {
    instance_id: "test".into(),
    role: "control",
  };
  let bound = Server::new(health, caller)
    .token(token.map(str::to_string))
    .runs(fake.clone())
    .processes(fake.clone())
    .guest(fake.clone())
    .runners(fake.clone())
    .clients(fake.clone())
    .bind(&[listen])
    .await
    .unwrap();
  let endpoint = match &bound.addresses()[0] {
    Listen::Unix(socket) => format!("unix:{}", socket.display()),
    Listen::Tcp(address) => format!("http://{address}"),
  };
  let cancel = CancellationToken::new();
  tokio::spawn(bound.serve(cancel.clone()));
  let client = Client::connect(&endpoint).await.unwrap();
  Served {
    fake,
    client,
    _dir: dir,
    _stop: cancel.drop_guard(),
  }
}
