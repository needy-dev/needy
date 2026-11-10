//! Workspace mounts: sshfs in passive mode, whose SFTP stream travels over a
//! needy exec stream (`Origin::ActorMount`) to `sftp-server` on the executor.
//! The daemon's relay may install the SFTP fault tap on that stream.

use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result};
use backon::{ConstantBuilder, Retryable};
use needy_api_client::Client;
use needy_api_client::exec::{Event, ExecStream};
use needy_api_proto::v1::{ExecStart, GuestMount, Origin};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Mounts an executor directory at `m.target`, owned by the user.
pub(super) async fn mount(client: &Client, m: &GuestMount, uid: u32, gid: u32) -> Result<()> {
  std::fs::create_dir_all(&m.target).with_context(|| format!("cannot create {}", m.target))?;
  let mut options = format!("passive,allow_other,uid={uid},gid={gid},idmap=none,cache=yes,kernel_cache,attr_timeout=1,entry_timeout=1");
  if !m.writable {
    options.push_str(",ro");
  }
  let mut sshfs = tokio::process::Command::new("sshfs")
    .args(["-f", "-o", &options, &format!(":{}", m.source), &m.target])
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::inherit())
    .spawn()
    .context("cannot start sshfs")?;
  let mut sshfs_in = sshfs.stdin.take().context("sshfs stdin")?;
  let mut sshfs_out = sshfs.stdout.take().context("sshfs stdout")?;
  let start = ExecStart {
    argv: m.sftp_command.clone(),
    executor: m.executor.clone(),
    // The mount root lets file faults match paths relative to it.
    cwd: m.source.clone(),
    origin: Origin::ActorMount.into(),
    ..Default::default()
  };
  let mut stream = ExecStream::start(client, start).await?;
  let input = stream.input();
  let target = m.target.clone();
  // sshfs's requests go to the executor's stdin.
  tokio::spawn(async move {
    // NOTICE: 256 KiB holds several of sshfs's largest SFTP writes (64 KiB
    // each by default).
    let mut buf = vec![0u8; 256 * 1024];
    loop {
      let n = match sshfs_out.read(&mut buf).await {
        Ok(0) => break,
        Ok(n) => n,
        Err(e) => {
          tracing::warn!("mount {target}: reading sshfs: {e}");
          break;
        }
      };
      if input.stdin(buf[..n].to_vec()).await.is_err() {
        break;
      }
    }
  });
  // The executor's replies go to sshfs.
  let target = m.target.clone();
  tokio::spawn(async move {
    loop {
      let event = match stream.next().await {
        Ok(Some(event)) => event,
        Ok(None) => break,
        Err(e) => {
          tracing::warn!("mount {target}: the SFTP stream failed: {e}");
          break;
        }
      };
      match event {
        Event::Stdout(bytes) => {
          if let Err(e) = async {
            sshfs_in.write_all(&bytes).await?;
            sshfs_in.flush().await
          }
          .await
          {
            tracing::warn!("mount {target}: writing to sshfs: {e}");
            break;
          }
        }
        Event::Exited(_) => break,
        Event::Detached => {
          // Mount streams never request detach; losing their replies ends
          // the mount instead of leaving sshfs waiting for an answer.
          tracing::warn!("mount {target}: the SFTP stream detached unexpectedly");
          break;
        }
        Event::Started(_) | Event::Stderr(_) | Event::Trace(_) => {}
      }
    }
  });
  let target = m.target.clone();
  tokio::spawn(async move {
    match sshfs.wait().await {
      Ok(status) if status.success() => tracing::debug!("mount {target}: sshfs exited"),
      Ok(status) => tracing::warn!("mount {target}: sshfs exited with {status}"),
      Err(e) => tracing::warn!("mount {target}: cannot wait for sshfs: {e}"),
    }
  });
  let target = m.target.trim_end_matches('/');
  // NOTICE: sshfs mounts after its first SFTP round trip; 100 retries
  // 100 ms apart allow 10 s for a slow executor.
  let retry = ConstantBuilder::default().with_delay(Duration::from_millis(100)).with_max_times(100);
  (|| async { if mounted(target) { Ok(()) } else { Err(()) } })
    .retry(retry)
    .await
    .map_err(|()| anyhow::anyhow!("sshfs did not mount within 10 s"))
}

/// Whether `/proc/mounts` lists `target`.
fn mounted(target: &str) -> bool {
  match std::fs::read_to_string("/proc/mounts") {
    Ok(mounts) => mounts.lines().any(|line| line.split(' ').nth(1) == Some(target)),
    Err(e) => {
      tracing::debug!("cannot read /proc/mounts: {e}");
      false
    }
  }
}
