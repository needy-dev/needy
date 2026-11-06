use crate::model::Input;
use anyhow::{Context, Result, ensure};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

async fn lines(
    reader: impl tokio::io::AsyncRead + Unpin,
    stream: &'static str,
    sender: mpsc::Sender<(&'static str, String)>,
) -> Result<()> {
    let mut reader = BufReader::new(reader);
    let mut pending = Vec::new();
    loop {
        let chunk = reader.fill_buf().await?;
        if chunk.is_empty() {
            if !pending.is_empty() {
                ensure!(
                    pending.len() <= crate::wire::MAX,
                    "runner output line exceeds 8 MiB"
                );
                sender.send((stream, String::from_utf8(pending)?)).await?;
            }
            return Ok(());
        }
        let count = chunk
            .iter()
            .position(|b| *b == b'\n')
            .map(|p| p + 1)
            .unwrap_or(chunk.len());
        ensure!(
            pending.len() + count <= crate::wire::MAX + 1,
            "runner output line exceeds 8 MiB"
        );
        pending.extend_from_slice(&chunk[..count]);
        reader.consume(count);
        if pending.last() == Some(&b'\n') {
            pending.pop();
            sender
                .send((stream, String::from_utf8(std::mem::take(&mut pending))?))
                .await?;
        }
    }
}

pub async fn run(input_dir: &Path) -> Result<i32> {
    execute(input_dir).await
}

async fn execute(input_dir: &Path) -> Result<i32> {
    let input: Input = serde_json::from_reader(std::fs::File::open(input_dir.join("input.json"))?)?;
    ensure!(
        !input.command.is_empty() && input.command.iter().all(|v| !v.contains('\0')),
        "invalid runner input"
    );
    let mut command = Command::new(&input.command[0]);
    command
        .args(&input.command[1..])
        .current_dir("/workspace")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command.as_std_mut().process_group(0);
    let mut child = command.spawn().context("runner cannot start workload")?;
    let pid = child.id().context("runner child has no PID")? as i32;
    let (sender, mut receiver) = mpsc::channel(32);
    let stdout = tokio::spawn(lines(
        child.stdout.take().unwrap(),
        "stdout",
        sender.clone(),
    ));
    let stderr = tokio::spawn(lines(
        child.stderr.take().unwrap(),
        "stderr",
        sender.clone(),
    ));
    drop(sender);
    let mut stdin = child.stdin.take().unwrap();
    let input_task = tokio::spawn(async move {
        match stdin.write_all(input.stdin.as_bytes()).await {
            Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => (),
            result => result?,
        }
        drop(stdin);
        Ok::<_, anyhow::Error>(())
    });
    let output = async {
        let mut total = 0usize;
        let mut stdout = tokio::io::stdout();
        while let Some((stream, line)) = receiver.recv().await {
            let mut bytes = serde_json::to_vec(&serde_json::json!({"stream":stream,"line":line}))?;
            bytes.push(b'\n');
            total += bytes.len();
            ensure!(total <= 16 << 20, "runner output exceeds 16 MiB");
            stdout.write_all(&bytes).await?;
        }
        stdout.flush().await?;
        Ok::<_, anyhow::Error>(())
    };
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    tokio::pin!(output);
    let mut drained = false;
    let status = loop {
        tokio::select! {
            result = child.wait() => break result?,
            result = &mut output, if !drained => {
                if let Err(error) = result { unsafe { libc::kill(-pid, libc::SIGKILL); } child.wait().await?; return Err(error); }
                drained=true;
            }
            _ = term.recv() => { unsafe { libc::kill(-pid, libc::SIGTERM); } }
            _ = interrupt.recv() => { unsafe { libc::kill(-pid, libc::SIGINT); } }
        }
    };
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
    }
    stdout.await??;
    stderr.await??;
    input_task.await??;
    if !drained {
        output.await?;
    }
    Ok(status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(libc::SIGKILL)))
}
