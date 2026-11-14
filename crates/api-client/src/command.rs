//! `exec:COMMAND` endpoints: each connection is a new process whose standard
//! input and output carry it, as ssh's `ProxyCommand` does. Any tool that
//! forwards stdio to a daemon's TCP port connects needy through it with no
//! code of its own here: `cloudflared access tcp --hostname H` (Cloudflare
//! Access), `ssh -W 127.0.0.1:7400 bastion`, `tailscale nc HOST 7400`.
//!
//! The command's standard error is the caller's, so its own messages (a
//! refused token, an unknown host) reach whoever runs needy, as with ssh's
//! `ProxyCommand`.
// Reference: https://man.openbsd.org/OpenBSD-7.8/ssh_config#ProxyCommand
// (read 2026-11-13: the command's stdin and stdout carry the connection).

use std::pin::Pin;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::process::{ChildStdin, ChildStdout};
use tokio::sync::oneshot;
use tonic::transport::{Channel, Endpoint, Uri};

use crate::{Error, Result};

type Observation = Arc<Mutex<Option<Error>>>;

/// A channel whose every connection runs `argv`.
pub(crate) async fn connect(endpoint: Endpoint, argv: Vec<String>) -> Result<Channel> {
  let observed = Observation::default();
  let connector_observed = observed.clone();
  let connected = endpoint
    .connect_with_connector(tower::service_fn(move |_: Uri| {
      let argv = argv.clone();
      let observed = connector_observed.clone();
      async move { spawn(&argv, observed).map(hyper_util::rt::TokioIo::new) }
    }))
    .await;
  let channel = match connected {
    Ok(channel) => channel,
    Err(error) => return Err(take_observed(&observed).await.unwrap_or(Error::Transport(error))),
  };
  // Establish protocol readiness while the spawning boundary still holds
  // its observation. Health is role-neutral and never starts work. Stderr
  // remains inherited; arbitrary vendor output is not classified or logged.
  let mut health = needy_api_proto::v1::health_service_client::HealthServiceClient::new(channel.clone());
  if let Err(status) = health.check(needy_api_proto::v1::CheckRequest {}).await {
    return Err(take_observed(&observed).await.unwrap_or(Error::Status(status)));
  }
  Ok(channel)
}

/// The connector's exit is recorded by its own task, which resolves as soon
/// as the process is reaped; the pipes can report the failure before that.
/// Wait briefly for the record before falling back to the transport error.
async fn take_observed(observed: &Observation) -> Option<Error> {
  for _ in 0..50 {
    if let Some(error) = observed.lock().unwrap_or_else(|e| e.into_inner()).take() {
      return Some(error);
    }
    tokio::time::sleep(Duration::from_millis(1)).await;
  }
  None
}

fn spawn(argv: &[String], observed: Observation) -> std::io::Result<Piped> {
  let (program, args) = argv.split_first().ok_or_else(|| std::io::Error::other("an empty endpoint command"))?;
  let mut child = tokio::process::Command::new(program)
    .args(args)
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::inherit())
    // The connection is the process: dropping the connection ends it.
    .kill_on_drop(true)
    .spawn()
    .map_err(|e| {
      let reason = if e.kind() == std::io::ErrorKind::NotFound {
        "connector.not_found"
      } else {
        "connector.start_failed"
      };
      *observed.lock().unwrap_or_else(|e| e.into_inner()) = Some(Error::Connector {
        reason,
        program: program.clone(),
        message: e.to_string(),
        exit_code: None,
      });
      std::io::Error::new(e.kind(), format!("cannot start the endpoint command {program}: {e}"))
    })?;
  let (Some(input), Some(output)) = (child.stdin.take(), child.stdout.take()) else {
    return Err(std::io::Error::other("the endpoint command has no standard input or output"));
  };
  let (kill, killed) = oneshot::channel();
  let name = program.clone();
  tokio::spawn(async move {
    let mut child = child;
    let waited = tokio::select! {
      _ = killed => None,
      status = child.wait() => Some(status),
    };
    let status = match waited {
      Some(status) => status,
      None => {
        let _ = child.start_kill();
        child.wait().await
      }
    };
    if let Ok(status) = status {
      *observed.lock().unwrap_or_else(|e| e.into_inner()) = Some(Error::Connector {
        reason: "connector.failed",
        program: name,
        message: format!("exited before a protocol connection was established ({status})"),
        exit_code: status.code(),
      });
    }
  });
  Ok(Piped {
    kill: Some(kill),
    input,
    output,
  })
}

/// One connection: reads come from the command's standard output, writes go
/// to its standard input. The command is killed when this is dropped.
struct Piped {
  kill: Option<oneshot::Sender<()>>,
  input: ChildStdin,
  output: ChildStdout,
}

impl Drop for Piped {
  fn drop(&mut self) {
    // The waiter is already gone once the command exited; the send then
    // fails and there is nothing left to kill.
    if let Some(kill) = self.kill.take() {
      let _ = kill.send(());
    }
  }
}

impl AsyncRead for Piped {
  fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
    Pin::new(&mut self.get_mut().output).poll_read(cx, buf)
  }
}

impl AsyncWrite for Piped {
  fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> {
    Pin::new(&mut self.get_mut().input).poll_write(cx, buf)
  }

  fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
    Pin::new(&mut self.get_mut().input).poll_flush(cx)
  }

  fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
    Pin::new(&mut self.get_mut().input).poll_shutdown(cx)
  }
}

#[cfg(all(test, unix))]
#[path = "command_test.rs"]
mod tests;
