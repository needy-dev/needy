use crate::{
    client::{self, Profile, Remote},
    daemon, model, process, store, wire, workspace,
};
use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use reqwest::Method;
use serde_json::{Value, json};
use std::path::PathBuf;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Parser)]
#[command(
    version,
    about = "Headless remote process runtime and cross-machine execution"
)]
pub struct Cli {
    #[arg(long,global=true,default_value_os_t=default_profile())]
    pub config: PathBuf,
    #[command(subcommand)]
    pub command: Commands,
}
fn default_profile() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").expect("HOME unset")).join(".config")
        })
        .join("needy/client.json")
}
#[derive(Subcommand)]
pub enum Commands {
    Daemon(Box<daemon::Config>),
    PairingCode {
        #[arg(long)]
        data_dir: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    Pair {
        #[arg(long)]
        url: String,
        #[arg(long)]
        code_file: Option<PathBuf>,
        #[arg(long, default_value = "client")]
        name: String,
        #[arg(long)]
        ca_cert: Option<PathBuf>,
    },
    Info,
    Sessions {
        #[command(subcommand)]
        action: Sessions,
    },
    Clients {
        #[command(subcommand)]
        action: Clients,
    },
    Workspace {
        #[command(subcommand)]
        action: Workspace,
    },
    Run {
        session: String,
        /// Read the command's standard input from FILE, or from Needy's stdin with `-`.
        #[arg(long, value_name = "FILE")]
        stdin: Option<PathBuf>,
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    Runs {
        #[command(subcommand)]
        action: Runs,
    },
    Events {
        run: String,
        #[arg(long, default_value_t = 0)]
        after: u64,
        #[arg(long)]
        follow: bool,
    },
    Stop {
        run: String,
    },
    Exec {
        #[arg(long)]
        cwd: PathBuf,
        #[arg(long)]
        pty: bool,
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    Intercept(crate::intercept::Options),
    #[command(hide = true)]
    Bridge {
        #[arg(long)]
        request: PathBuf,
    },
    #[command(hide = true)]
    ProcessWorker {
        #[arg(long)]
        request: PathBuf,
    },
    #[command(hide = true)]
    Runner {
        #[arg(long, default_value = "/run/needy")]
        input_dir: PathBuf,
    },
}
#[derive(Subcommand)]
pub enum Sessions {
    Create {
        #[arg(long, default_value = "")]
        name: String,
    },
    List,
    Show {
        id: String,
    },
    Delete {
        id: String,
    },
}
#[derive(Subcommand)]
pub enum Clients {
    List,
    Revoke { id: String },
}
#[derive(Subcommand)]
pub enum Workspace {
    Push { session: String, directory: PathBuf },
    Pull { session: String, directory: PathBuf },
}
#[derive(Subcommand)]
pub enum Runs {
    Show { id: String },
}

async fn print(remote: &Remote, method: Method, path: &str, body: Option<Value>) -> Result<()> {
    let value: Value = remote.request(method, path, body).await?.json().await?;
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}
fn segment(value: &str) -> Result<&str> {
    ensure!(
        !value.is_empty()
            && value
                .bytes()
                .all(|v| v.is_ascii_alphanumeric() || v == b'-' || v == b'_'),
        "invalid resource ID"
    );
    Ok(value)
}

pub async fn run(cli: Cli) -> Result<i32> {
    let config = cli.config;
    match cli.command {
        Commands::Daemon(config) => {
            daemon::serve(*config).await?;
        }
        Commands::PairingCode { data_dir, out } => {
            daemon::pairing_code(&data_dir, &out)?;
        }
        Commands::Pair {
            url,
            code_file,
            name,
            ca_cert,
        } => {
            let code = match code_file {
                Some(file) => std::fs::read_to_string(file)?,
                None => {
                    let mut code = String::new();
                    tokio::io::stdin().read_to_string(&mut code).await?;
                    code
                }
            };
            let ca_cert = ca_cert.map(std::fs::canonicalize).transpose()?;
            let http = client::transport(&url, ca_cert.as_deref())?;
            let response = client::check(
                http.post(format!("{}/v1/pair", url.trim_end_matches('/')))
                    .json(&json!({"code":code.trim(),"name":name}))
                    .send()
                    .await?,
            )
            .await?;
            let value: Value = response.json().await?;
            let profile = Profile {
                url,
                token: value["token"]
                    .as_str()
                    .context("pairing response missing token")?
                    .into(),
                client_id: value["client_id"]
                    .as_str()
                    .context("pairing response missing client ID")?
                    .into(),
                ca_cert,
            };
            store::private_directory(config.parent().context("client profile has no parent")?)?;
            store::atomic_json(&config, &profile)?;
            println!("{}", json!({"paired":true,"client_id":profile.client_id}));
        }
        Commands::ProcessWorker { request } => {
            let input: process::Request =
                serde_json::from_slice(&workspace::read_private(&request)?)?;
            match process::worker(input).await {
                Ok(code) => return Ok(code),
                Err(error) => {
                    wire::write(
                        &mut tokio::io::stdout(),
                        wire::FAILURE,
                        &serde_json::to_vec(&json!({"error":"host process failed"}))?,
                    )
                    .await?;
                    return Err(error);
                }
            }
        }
        Commands::Runner { input_dir } => match crate::runner::run(&input_dir).await {
            Ok(code) => return Ok(code),
            Err(error) => {
                let envelope = serde_json::to_vec(
                    &json!({"stream":"stderr","line":"runner failed; invalid input, output or process lifecycle"}),
                )?;
                let mut output = tokio::io::stdout();
                output.write_all(&envelope).await?;
                output.write_all(b"\n").await?;
                output.flush().await?;
                eprintln!(
                    "{}",
                    serde_json::to_string(
                        &json!({"stream":"stderr","line":format!("runner failure: {error:#}")})
                    )?
                );
                return Ok(125);
            }
        },
        Commands::Intercept(options) => return crate::intercept::run(config, options).await,
        Commands::Bridge { request } => return crate::intercept::bridge(&request).await,
        command => {
            let remote = Remote::open(&config)?;
            match command {
                Commands::Info => print(&remote, Method::GET, "/v1/info", None).await?,
                Commands::Sessions { action } => match action {
                    Sessions::Create { name } => {
                        print(
                            &remote,
                            Method::POST,
                            "/v1/sessions",
                            Some(json!({"name":name})),
                        )
                        .await?
                    }
                    Sessions::List => print(&remote, Method::GET, "/v1/sessions", None).await?,
                    Sessions::Show { id } => {
                        print(
                            &remote,
                            Method::GET,
                            &format!("/v1/sessions/{}", segment(&id)?),
                            None,
                        )
                        .await?
                    }
                    Sessions::Delete { id } => {
                        print(
                            &remote,
                            Method::DELETE,
                            &format!("/v1/sessions/{}", segment(&id)?),
                            None,
                        )
                        .await?
                    }
                },
                Commands::Clients { action } => match action {
                    Clients::List => print(&remote, Method::GET, "/v1/clients", None).await?,
                    Clients::Revoke { id } => {
                        print(
                            &remote,
                            Method::DELETE,
                            &format!("/v1/clients/{}", segment(&id)?),
                            None,
                        )
                        .await?
                    }
                },
                Commands::Workspace { action } => match action {
                    Workspace::Push { session, directory } => {
                        let bytes = workspace::pack(&directory)?;
                        let response = remote
                            .http
                            .put(format!(
                                "{}/v1/sessions/{}/workspace",
                                remote.profile.url.trim_end_matches('/'),
                                segment(&session)?
                            ))
                            .bearer_auth(&remote.profile.token)
                            .header("content-type", "application/x-tar")
                            .body(bytes)
                            .send()
                            .await?;
                        client::check(response).await?;
                        println!("{}", json!({"uploaded":session}));
                    }
                    Workspace::Pull { session, directory } => {
                        let response = remote
                            .request(
                                Method::GET,
                                &format!("/v1/sessions/{}/workspace", segment(&session)?),
                                None,
                            )
                            .await?;
                        let bytes = response.bytes().await?;
                        workspace::unpack(&bytes, &directory)?;
                        println!("{}", json!({"downloaded":session}));
                    }
                },
                Commands::Run {
                    session,
                    stdin,
                    command,
                } => {
                    let stdin = match stdin {
                        None => String::new(),
                        Some(path) if path.as_os_str() == "-" => {
                            let mut text = String::new();
                            tokio::io::stdin().read_to_string(&mut text).await?;
                            text
                        }
                        Some(path) => std::fs::read_to_string(path)?,
                    };
                    ensure!(stdin.len() <= 16 << 20, "stdin exceeds 16 MiB");
                    let request = json!({"command":command,"stdin":stdin});
                    let response = remote
                        .http
                        .post(format!(
                            "{}/v1/sessions/{}/runs",
                            remote.profile.url.trim_end_matches('/'),
                            segment(&session)?
                        ))
                        .bearer_auth(&remote.profile.token)
                        .header("idempotency-key", model::id())
                        .json(&request)
                        .send()
                        .await?;
                    let value: Value = client::check(response).await?.json().await?;
                    println!("{}", serde_json::to_string_pretty(&value)?);
                }
                Commands::Runs {
                    action: Runs::Show { id },
                } => {
                    print(
                        &remote,
                        Method::GET,
                        &format!("/v1/runs/{}", segment(&id)?),
                        None,
                    )
                    .await?
                }
                Commands::Stop { run } => {
                    print(
                        &remote,
                        Method::POST,
                        &format!("/v1/runs/{}/stop", segment(&run)?),
                        None,
                    )
                    .await?
                }
                Commands::Events { run, after, follow } => {
                    use futures_util::StreamExt;
                    let response = remote
                        .request(
                            Method::GET,
                            &format!(
                                "/v1/runs/{}/events?after={after}&follow={follow}",
                                segment(&run)?
                            ),
                            None,
                        )
                        .await?;
                    let mut chunks = response.bytes_stream();
                    let mut output = tokio::io::stdout();
                    while let Some(bytes) = chunks.next().await {
                        output.write_all(&bytes?).await?;
                        output.flush().await?;
                    }
                }
                Commands::Exec { cwd, pty, command } => {
                    let request = process::Request {
                        command,
                        argv0: None,
                        cwd,
                        environment: std::collections::BTreeMap::new(),
                        pty: if pty { Some(client::window()?) } else { None },
                    };
                    process::validate(&request)?;
                    let socket = remote.connect("/v1/exec", wire::EXEC, &request).await?;
                    return client::tunnel(socket, pty).await?.shell_code();
                }
                _ => unreachable!(),
            }
        }
    }
    Ok(0)
}
