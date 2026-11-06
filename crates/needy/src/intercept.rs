use crate::{
    client::{self, Remote},
    process, wire,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, atomic::AtomicBool};

#[derive(clap::Args)]
pub struct Options {
    #[arg(long, required = true, value_delimiter = ',')]
    pub select: Vec<String>,
    #[arg(long)]
    pub remote_cwd: PathBuf,
    #[arg(long, value_delimiter = ',')]
    pub forward_env: Vec<String>,
    #[arg(long)]
    pub no_pty: bool,
    #[arg(last = true, required = true)]
    pub command: Vec<OsString>,
}
#[derive(Serialize, Deserialize)]
struct BridgeRequest {
    profile: PathBuf,
    request: process::Request,
    allow_pty: bool,
}

pub async fn run(config: PathBuf, options: Options) -> Result<i32> {
    ensure!(
        cfg!(target_os = "linux"),
        "exec interception requires Linux"
    );
    ensure!(
        options.remote_cwd.is_absolute(),
        "remote cwd must be absolute"
    );
    // Validate the profile before stopping any user process.
    Remote::open(&config)?;
    // A cancelled proxy is killed before its own raw-mode guard can run.
    // The surviving supervisor restores the shared terminal in that case.
    let _terminal = if unsafe { libc::isatty(0) } == 1 {
        Some(client::Terminal::save()?)
    } else {
        None
    };
    let config = std::fs::canonicalize(config)?;
    let executable = std::env::current_exe()?;
    let cancel = Arc::new(AtomicBool::new(false));
    let mut registrations = Vec::new();
    for signal in [libc::SIGTERM, libc::SIGHUP] {
        registrations.push(signal_hook::flag::register(signal, cancel.clone())?);
    }
    // SIGINT also reaches the foreground tracee. Keep the supervisor alive so
    // it can preserve that process's signal/exit state and restore its terminal.
    registrations.push(signal_hook::flag::register(
        libc::SIGINT,
        Arc::new(AtomicBool::new(false)),
    )?);
    let result = tokio::task::spawn_blocking(move || -> Result<i32> {
        let directory = tempfile::Builder::new().prefix("needy-exec-").tempdir()?;
        let mut command = needy_exec_intercept::Command::new(PathBuf::from(&options.command[0]))?;
        command.args = options.command[1..].to_vec();
        let report = needy_exec_intercept::supervise(command, &cancel, |exec| {
            let result = (|| -> Result<needy_exec_intercept::Action> {
                let path = if exec.executable.is_empty() {
                    if let Some(fd) = exec.dir_fd {
                        std::fs::read_link(format!("/proc/{}/fd/{fd}", exec.pid))?
                    } else {
                        return Ok(needy_exec_intercept::Action::Continue);
                    }
                } else {
                    PathBuf::from(&exec.executable)
                };
                let name = path
                    .file_name()
                    .and_then(|v| v.to_str())
                    .context("executable identity is not UTF-8")?;
                if !options.select.iter().any(|value| value == name) {
                    return Ok(needy_exec_intercept::Action::Continue);
                }
                let mut environment = std::collections::BTreeMap::new();
                for entry in &exec.environment {
                    let bytes = entry.as_bytes();
                    let Some(index) = bytes.iter().position(|b| *b == b'=') else {
                        continue;
                    };
                    let Ok(key) = std::str::from_utf8(&bytes[..index]) else {
                        continue;
                    };
                    if options.forward_env.iter().any(|name| name == key) {
                        environment.insert(
                            key.into(),
                            std::str::from_utf8(&bytes[index + 1..])
                                .context("forwarded environment is not UTF-8")?
                                .into(),
                        );
                    }
                }
                let mut command = vec![name.into()];
                command.extend(
                    exec.argv
                        .iter()
                        .skip(1)
                        .map(|value| {
                            value
                                .to_str()
                                .map(String::from)
                                .context("remote exec arguments must be UTF-8")
                        })
                        .collect::<Result<Vec<_>>>()?,
                );
                let request = process::Request {
                    command,
                    argv0: exec
                        .argv
                        .first()
                        .map(|value| {
                            value
                                .to_str()
                                .map(String::from)
                                .context("remote argv[0] must be UTF-8")
                        })
                        .transpose()?,
                    cwd: options.remote_cwd.clone(),
                    environment,
                    pty: None,
                };
                process::validate(&request)?;
                let file = directory.path().join(format!(
                    "{}-{}.json",
                    exec.pid,
                    uuid::Uuid::new_v4().simple()
                ));
                crate::store::atomic_json(
                    &file,
                    &BridgeRequest {
                        profile: config.clone(),
                        request,
                        allow_pty: !options.no_pty,
                    },
                )?;
                Ok(needy_exec_intercept::Action::Redirect(
                    needy_exec_intercept::Replacement {
                        executable: executable.clone(),
                        argv: vec![
                            executable.as_os_str().to_owned(),
                            "bridge".into(),
                            "--request".into(),
                            file.into_os_string(),
                        ],
                        environment: None,
                    },
                ))
            })();
            result.map_err(|error| std::io::Error::other(error.to_string()))
        })?;
        eprintln!(
            "needy exec: {} redirected, {} exec events, {} targeted injection stops",
            report.redirected, report.exec_events, report.injection_stops
        );
        Ok(report.outcome.shell_code())
    })
    .await?;
    for registration in registrations {
        signal_hook::low_level::unregister(registration);
    }
    result
}

pub async fn bridge(path: &Path) -> Result<i32> {
    let mut input: BridgeRequest = serde_json::from_slice(&crate::workspace::read_private(path)?)?;
    std::fs::remove_file(path)?;
    let pty = input.allow_pty && unsafe { libc::isatty(0) } == 1 && unsafe { libc::isatty(1) } == 1;
    if pty {
        input.request.pty = Some(client::window()?);
        let term = std::env::var("TERM").context("TERM is required for PTY execution")?;
        input.request.environment.insert("TERM".into(), term);
    }
    let remote = Remote::open(&input.profile)?;
    let socket = remote
        .connect("/v1/exec", wire::EXEC, &input.request)
        .await?;
    let exit = client::tunnel(socket, pty).await?;
    if let Some(name) = &exit.signal {
        let signal = process::signal_number(name)?;
        unsafe {
            if signal != libc::SIGKILL {
                libc::signal(signal, libc::SIG_DFL);
            }
            let mut mask = std::mem::zeroed();
            libc::sigemptyset(&mut mask);
            libc::sigaddset(&mut mask, signal);
            libc::pthread_sigmask(libc::SIG_UNBLOCK, &mask, std::ptr::null_mut());
            libc::raise(signal);
        }
        anyhow::bail!("remote death signal was not delivered locally");
    }
    Ok(exit.code)
}
