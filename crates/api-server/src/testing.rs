//! A fake daemon for the adapters' tests: it implements the domain traits,
//! records what it was asked, and answers predictably. Tests drive it
//! through a real [`Server`] and `needy-api-client`.

use std::collections::HashMap;
use std::sync::Mutex;

use needy_api_client::Client;
use needy_common::process::Exit;
use needy_core::event::{Event, SessionStop};
use needy_core::intercept::ExecContext;
use needy_core::types::SessionState;
use tokio_stream::StreamExt as _;
use tokio_util::sync::CancellationToken;

use crate::control::*;
use crate::protocol::grpc::v1::health::Health;
use crate::server::{Listen, Server};

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
  Event::SessionStop(SessionStop {})
}

fn info(id: &str, name: &str) -> SessionInfo {
  SessionInfo {
    id: id.into(),
    name: name.into(),
    group: id.into(),
    state: SessionState::Running,
    executors: Vec::new(),
    created_ms: 1,
    profile: "fake".into(),
  }
}

#[async_trait::async_trait]
impl Sessions for Fake {
  async fn create(&self, request: NewSession) -> Result<SessionInfo> {
    self.call(format!("create {}", request.profile_name));
    Ok(info("s1", &request.name))
  }

  async fn get(&self, id: &str) -> Result<SessionInfo> {
    match id {
      "s1" => Ok(info("s1", "one")),
      _ => Err(ControlError::NotFound("no such session".into())),
    }
  }

  async fn list(&self) -> Result<Vec<SessionInfo>> {
    Ok(vec![info("s1", "one"), info("s2", "two")])
  }

  async fn stop(&self, id: &str) -> Result<()> {
    self.call(format!("stop {id}"));
    Ok(())
  }

  async fn logs(&self, id: &str) -> Result<Vec<u8>> {
    self.call(format!("logs {id}"));
    // Larger than one message, so the adapter must chunk it.
    Ok(vec![b'c'; 3 << 20])
  }

  async fn decide(&self, session: &str, ctx: ExecContext) -> Result<Route> {
    self.call(format!("decide {session}"));
    self.contexts.lock().unwrap().push(ctx.clone());
    let target = match ctx.argv.first().map(String::as_str) {
      Some("rm") => Target::Refused("denied: no deleting".into()),
      Some("ls") => Target::Actor,
      _ => Target::Executor(format!("on-{session}")),
    };
    Ok(Route {
      target,
      argv: ctx.argv,
      env: HashMap::from([("SET".into(), "1".into())]),
      clear_env: false,
      cwd: ctx.cwd,
    })
  }
}

#[async_trait::async_trait]
impl Processes for Fake {
  /// Echoes the stdin of a start until end of input, then exits 3. The
  /// started process's executor names the session it was placed in.
  async fn exec(&self, open: ExecOpen, mut input: Inputs) -> Result<Outputs> {
    let (process, executor) = match open {
      ExecOpen::Start(spec) => {
        self.call(format!("exec {} in {:?} from {:?}", spec.argv.join(" "), spec.session, spec.origin));
        if spec.argv.first().map(String::as_str) == Some("refuse") {
          return Err(ControlError::Denied {
            message: "denied: refused".into(),
            events: vec![refusal_event()],
          });
        }
        if spec.argv.first().map(String::as_str) == Some("full") {
          return Err(ControlError::Exhausted("executor is full".into()));
        }
        ("p1".to_string(), spec.session)
      }
      ExecOpen::Attach { process, replay } => {
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
          ExecInput::Signal(_) | ExecInput::Resize(_) => {}
        }
      }
      echoed
    });
    Ok(stream)
  }

  async fn list(&self, session: &str) -> Result<Vec<ProcessInfo>> {
    self.call(format!("list {session:?}"));
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
      session: session.into(),
    }])
  }

  async fn kill(&self, process: &str, signal: &str) -> Result<()> {
    self.call(format!("kill {process} {signal}"));
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
  async fn config(&self, session: &str) -> Result<GuestConfig> {
    Ok(GuestConfig {
      session: session.into(),
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

  async fn event(&self, session: &str, time_ms: i64, event: Event) -> Result<()> {
    self.events.lock().unwrap().push((session.into(), time_ms, event));
    Ok(())
  }

  async fn exit(&self, session: &str, exit: Exit) -> Result<()> {
    self.exits.lock().unwrap().push((session.into(), exit));
    Ok(())
  }

  async fn fetch_files(&self, _session: &str, name: &str) -> Result<Vec<u8>> {
    match name {
      "big" => Ok(vec![b'f'; 3 << 20]),
      _ => Err(ControlError::NotFound("no such copy-in mapping".into())),
    }
  }

  async fn push_files(&self, session: &str, name: &str, archive: Vec<u8>) -> Result<()> {
    self.pushed.lock().unwrap().push((session.into(), name.into(), archive));
    Ok(())
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

pub async fn serve_with(caller: Caller, token: Option<&str>) -> Served {
  let dir = tempfile::tempdir().unwrap();
  let socket = dir.path().join("api.sock");
  let fake = std::sync::Arc::new(Fake::default());
  let health = Health {
    instance_id: "test".into(),
    role: "control",
  };
  let bound = Server::new(health, caller)
    .token(token.map(str::to_string))
    .sessions(fake.clone())
    .processes(fake.clone())
    .guest(fake.clone())
    .bind(&[Listen::Unix(socket.clone())])
    .await
    .unwrap();
  let cancel = CancellationToken::new();
  tokio::spawn(bound.serve(cancel.clone()));
  let client = Client::connect(&format!("unix:{}", socket.display())).await.unwrap();
  Served {
    fake,
    client,
    _dir: dir,
    _stop: cancel.drop_guard(),
  }
}
