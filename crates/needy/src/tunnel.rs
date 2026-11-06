use crate::{
    daemon::{self, Active, Api, Shared},
    model, process, store, wire,
};
use anyhow::{Context, Result};
use axum::{
    body::to_bytes,
    extract::{Request, State},
    http::{StatusCode, Version},
    response::Response,
};
use hyper::upgrade::OnUpgrade;
use hyper_util::rt::TokioIo;
use std::process::Stdio;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite},
    process::Command,
    sync::watch,
};

fn upgrade(request: &mut Request, protocol: &str, id: &str) -> Api<(OnUpgrade, Response)> {
    let headers = request.headers();
    if request.version() != Version::HTTP_11
        || !headers
            .get("upgrade")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.eq_ignore_ascii_case(protocol))
        || !headers
            .get("connection")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| {
                v.split(',')
                    .any(|v| v.trim().eq_ignore_ascii_case("upgrade"))
            })
    {
        return Err(daemon::problem(
            StatusCode::UPGRADE_REQUIRED,
            "authenticated HTTP/1.1 upgrade required",
        ));
    }
    let stream = hyper::upgrade::on(request);
    let response = Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header("connection", "Upgrade")
        .header("upgrade", protocol)
        .header("x-needy-run-id", id)
        .body(axum::body::Body::empty())
        .map_err(anyhow::Error::from)?;
    Ok((stream, response))
}

pub async fn exec_upgrade(State(s): State<Shared>, mut request: Request) -> Api<Response> {
    let owner = s.authorize(request.headers()).await?;
    if s.host_commands.is_empty() {
        return Err(daemon::problem(
            StatusCode::CONFLICT,
            "host execution is disabled",
        ));
    }
    let id = model::id();
    let (upgrade, response) = upgrade(&mut request, wire::EXEC, &id)?;
    let mut input: process::Request = serde_json::from_slice(
        &to_bytes(request.into_body(), 2 << 20)
            .await
            .map_err(anyhow::Error::from)?,
    )
    .map_err(|_| daemon::problem(StatusCode::BAD_REQUEST, "invalid process request"))?;
    process::validate(&input)
        .map_err(|e| daemon::problem(StatusCode::BAD_REQUEST, &e.to_string()))?;
    let executable = s
        .host_commands
        .get(&input.command[0])
        .ok_or_else(|| daemon::problem(StatusCode::FORBIDDEN, "host command not allowed"))?;
    input.command[0] = executable
        .to_str()
        .context("host executable path is not UTF-8")?
        .into();
    let cwd = std::fs::canonicalize(&input.cwd)
        .map_err(|_| daemon::problem(StatusCode::BAD_REQUEST, "host cwd does not exist"))?;
    if !cwd.starts_with(s.config.host_root.as_ref().unwrap()) {
        return Err(daemon::problem(
            StatusCode::FORBIDDEN,
            "host cwd outside configured root",
        ));
    }
    input.cwd = cwd;
    if input
        .environment
        .keys()
        .any(|key| !s.config.host_env.contains(key))
    {
        return Err(daemon::problem(
            StatusCode::FORBIDDEN,
            "host environment variable not allowed",
        ));
    }
    let (cancel, receiver) = watch::channel(false);
    {
        let disk = s.disk.lock().await;
        if !disk.clients.values().any(|c| c.token_hash == owner) {
            return Err(daemon::problem(StatusCode::UNAUTHORIZED, "client revoked"));
        }
        s.capacity(&disk).await?;
        s.active.lock().await.insert(
            id.clone(),
            Active {
                owner,
                cancel,
                native: true,
            },
        );
    }
    tokio::spawn(async move {
        let result = async {
            let socket = TokioIo::new(upgrade.await?);
            native_connection(&s, &id, input, socket, receiver).await
        }
        .await;
        if let Err(error) = result {
            eprintln!("host execution ended: {error:#}");
        }
        s.active.lock().await.remove(&id);
    });
    Ok(response)
}

async fn native_connection(
    s: &Shared,
    id: &str,
    input: process::Request,
    socket: impl AsyncRead + AsyncWrite + Unpin,
    cancel: watch::Receiver<bool>,
) -> Result<()> {
    let directory = s.config.data_dir.join("process-inputs");
    store::private_directory(&directory)?;
    let file = tempfile::NamedTempFile::new_in(directory)?;
    store::atomic_json(file.path(), &input)?;
    let mut child = Command::new(std::env::current_exe()?)
        .arg("process-worker")
        .arg("--request")
        .arg(file.path())
        .kill_on_drop(true)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let diagnostics = tokio::spawn(async move {
        let mut bytes = Vec::new();
        stderr.take(65536).read_to_end(&mut bytes).await?;
        Ok::<_, std::io::Error>(bytes)
    });
    let (mut reader, mut writer) = tokio::io::split(socket);
    let result = {
        let incoming = tokio::io::copy(&mut reader, &mut stdin);
        let outgoing = async {
            loop {
                let (kind, data) = wire::read(&mut stdout).await?;
                if kind == wire::EXIT {
                    return Ok::<_, anyhow::Error>(Some(data));
                }
                wire::write(&mut writer, kind, &data).await?;
            }
        };
        tokio::pin!(incoming, outgoing);
        tokio::select! {
            result=&mut incoming=>result.map(|_|None).map_err(anyhow::Error::from),
            result=&mut outgoing=>result,
            _=cancelled(cancel)=>Ok(None),
        }
    };
    // Dropping this pipe tells the worker to kill and reap its process group.
    drop(stdin);
    tokio::time::timeout(std::time::Duration::from_secs(5), child.wait())
        .await
        .context("host worker did not clean up")??;
    diagnostics.await??;
    let exit = result?;
    s.active.lock().await.remove(id);
    if let Some(exit) = exit {
        wire::write(&mut writer, wire::EXIT, &exit).await?;
    }
    Ok(())
}

async fn cancelled(mut receiver: watch::Receiver<bool>) {
    if *receiver.borrow() {
        return;
    }
    let _ = receiver.changed().await;
}
