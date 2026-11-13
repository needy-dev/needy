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
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tonic::transport::{Channel, Endpoint, Uri};

use crate::Result;

/// A channel whose every connection runs `argv`.
pub(crate) async fn connect(endpoint: Endpoint, argv: Vec<String>) -> Result<Channel> {
  Ok(
    endpoint
      .connect_with_connector(tower::service_fn(move |_: Uri| {
        let argv = argv.clone();
        async move { spawn(&argv).map(hyper_util::rt::TokioIo::new) }
      }))
      .await?,
  )
}

fn spawn(argv: &[String]) -> std::io::Result<Piped> {
  let (program, args) = argv.split_first().ok_or_else(|| std::io::Error::other("an empty endpoint command"))?;
  let mut child = tokio::process::Command::new(program)
    .args(args)
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::inherit())
    // The connection is the process: dropping the connection ends it.
    .kill_on_drop(true)
    .spawn()
    .map_err(|e| std::io::Error::new(e.kind(), format!("cannot start the endpoint command {program}: {e}")))?;
  let (Some(input), Some(output)) = (child.stdin.take(), child.stdout.take()) else {
    return Err(std::io::Error::other("the endpoint command has no standard input or output"));
  };
  Ok(Piped {
    _child: child,
    input,
    output,
  })
}

/// One connection: reads come from the command's standard output, writes go
/// to its standard input. The command is killed when this is dropped.
struct Piped {
  _child: Child,
  input: ChildStdin,
  output: ChildStdout,
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
