//! The `needy` binary: CLI, daemon, executor, and the actor guest supervisor and proxy.

mod bench;
mod build;
mod chaos;
#[cfg(unix)]
mod control;
mod exec_cli;
mod executor;
mod files;
mod fs_cli;
mod gateway;
mod guest;
mod prochost;
mod report;
mod server;
mod trace;

use anyhow::Result;
use clap::{Args, Parser, Subcommand, ValueEnum};
use needy_api_proto::v1::health_service_server::HealthServiceServer;
use needy_api_proto::v1::process_service_server::ProcessServiceServer;

#[derive(Parser)]
#[command(
    name = "needy",
    version,
    about = "Route a driving program's work to executors"
)]
struct Cli {
    /// Daemon endpoint: unix:/path, http://host:port or https://host:port.
    #[arg(long, global = true, env = "NEEDY_ENDPOINT")]
    endpoint: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run a daemon.
    Daemon(DaemonArgs),
    /// Run a command through placement.
    Exec(ExecArgs),
    /// List processes.
    Ps {
        #[arg(long, default_value = "")]
        session: String,
    },
    /// Reattach to a process, replaying recent output first.
    Attach {
        process_id: String,
        /// Do not replay buffered output.
        #[arg(long)]
        no_replay: bool,
    },
    /// Send a signal to a process.
    Kill {
        process_id: String,
        #[arg(short, long, default_value = "SIGTERM")]
        signal: String,
    },
    /// Copy files between this machine and executors.
    #[command(subcommand)]
    Fs(FsCommand),
    /// Manage sessions: one actor and its executors.
    #[command(subcommand)]
    Session(SessionCommand),
    /// Build an actor image from a recipe, and the executor image.
    Build {
        #[arg(long)]
        recipe: Option<std::path::PathBuf>,
        /// Output directory for vmlinux, rootfs.ext4 and firecracker.
        #[arg(long)]
        out: Option<std::path::PathBuf>,
        /// Also build the `needy-executor` Docker image.
        #[arg(long)]
        executor: bool,
    },
    /// Latency and throughput probes; each prints one JSON line.
    #[command(subcommand, hide = true)]
    Bench(BenchCommand),
    /// Check a profile: show routing decisions and the recipe's findings.
    #[command(subcommand)]
    Profile(ProfileCommand),
    /// Write a markdown report of a session.
    Report {
        /// Session ID prefix, or `latest`.
        #[arg(default_value = "latest")]
        session: String,
        #[arg(long, value_enum, default_value = "summary")]
        level: ReportLevel,
        #[arg(long)]
        output: Option<std::path::PathBuf>,
    },
}

#[derive(Subcommand)]
enum FsCommand {
    /// Copy a file or directory into a directory: `SRC EXECUTOR:DIR` or `EXECUTOR:PATH DIR`.
    Cp {
        source: String,
        destination: String,
        #[arg(long, default_value = "")]
        session: String,
    },
}

#[derive(Subcommand)]
enum BenchCommand {
    /// Spawn a command repeatedly and measure spawn-to-exit time.
    Spawn {
        #[arg(short = 'n', default_value_t = 100)]
        count: usize,
        #[arg(long, default_value = "")]
        label: String,
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Read a command's stdout and measure throughput.
    Pipe {
        #[arg(long, default_value = "")]
        label: String,
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Measure file throughput and small-file operations in a directory.
    Fs {
        dir: std::path::PathBuf,
        #[arg(long, default_value_t = 256)]
        mib: usize,
        #[arg(long, default_value_t = 1000)]
        files: usize,
        #[arg(long, default_value = "")]
        label: String,
    },
    /// Run several probes in sequence; each argument is one quoted probe.
    Suite {
        #[arg(required = true)]
        probes: Vec<String>,
    },
}

#[derive(Subcommand)]
enum ProfileCommand {
    /// Load a profile and print where each command would run.
    Check {
        profile: std::path::PathBuf,
        /// Commands to decide, each as one shell-quoted string.
        #[arg(long = "cmd")]
        commands: Vec<String>,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum ReportLevel {
    Summary,
    Detailed,
}

#[derive(Subcommand)]
enum SessionCommand {
    /// Create a session from a profile and start its actor.
    Create {
        #[arg(long)]
        profile: std::path::PathBuf,
        #[arg(long, default_value = "")]
        group: String,
        #[arg(long, default_value = "")]
        name: String,
        /// Create executors only; do not start the actor.
        #[arg(long)]
        no_actor: bool,
        /// Override the actor's driving program.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// List sessions.
    Ls,
    /// Stop a session and release its executors.
    Stop { session: String },
}

#[derive(Clone, Copy, ValueEnum)]
enum Mode {
    /// Run processes on this machine only.
    Executor,
    /// Manage sessions, actors and executors.
    Control,
}

#[derive(Args)]
struct DaemonArgs {
    #[arg(long, value_enum, default_value = "executor")]
    mode: Mode,
    /// Where to listen: unix:/path or HOST:PORT.
    #[arg(long)]
    listen: Option<String>,
    /// Executor name reported to clients.
    #[arg(long, default_value = "local")]
    name: String,
    /// Environment variable holding the bearer token clients must present.
    #[arg(long)]
    token_env: Option<String>,
    /// State directory of a control daemon.
    #[arg(long)]
    state_dir: Option<std::path::PathBuf>,
    /// Directory with the actor's vmlinux, rootfs.ext4 and firecracker.
    #[arg(long)]
    actor_image: Option<std::path::PathBuf>,
    /// Docker image for docker executors without their own image.
    #[arg(long, default_value = "needy-executor:latest")]
    docker_image: String,
}

#[derive(Args)]
struct ExecArgs {
    /// Force a terminal.
    #[arg(short = 't', conflicts_with = "no_tty")]
    tty: bool,
    /// Force pipes; stdout and stderr stay separate.
    #[arg(short = 'T')]
    no_tty: bool,
    /// Do not forward stdin.
    #[arg(short = 'n')]
    no_stdin: bool,
    /// Start the process, print its ID and return.
    #[arg(short = 'd')]
    detach: bool,
    #[arg(long)]
    cwd: Option<String>,
    /// Set an environment variable, KEY=VALUE.
    #[arg(short = 'e', long = "env")]
    env: Vec<String>,
    /// Start from an empty environment.
    #[arg(long)]
    clear_env: bool,
    #[arg(long, default_value = "")]
    session: String,
    #[arg(long, default_value = "")]
    executor: String,
    #[arg(long, value_enum, default_value = "human")]
    output: exec_cli::OutputMode,
    #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
    command: Vec<String>,
}

fn default_endpoint() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    format!("unix:{home}/.needy/daemon.sock")
}

fn needy_home() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    std::path::PathBuf::from(home).join(".needy")
}

fn main() -> Result<()> {
    // Inside an actor the same binary is the init process and the exec proxy.
    // The kernel starts init before /proc is mounted, so check argv[0]; a
    // container's PID 1 is not an actor's init.
    if std::env::args_os().next().as_deref() == Some(std::ffi::OsStr::new("/.needy/init")) {
        guest::init_main();
    }
    if let Ok(exe) = std::env::current_exe() {
        match exe.to_str() {
            Some("/.needy/proxy") => guest::proxy_main(),
            Some("/.needy/init") => guest::init_main(),
            _ => {}
        }
    }
    let cli = Cli::parse();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let endpoint = cli.endpoint.clone().unwrap_or_else(default_endpoint);
    let code = runtime.block_on(async move {
        match cli.command {
            Command::Daemon(args) => daemon(args, &endpoint).await.map(|_| 0),
            Command::Exec(args) => {
                let env = args
                    .env
                    .iter()
                    .filter_map(|kv| kv.split_once('='))
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect();
                exec_cli::exec(
                    &endpoint,
                    exec_cli::ExecOptions {
                        argv: args.command,
                        cwd: args.cwd,
                        env,
                        clear_env: args.clear_env,
                        tty: if args.tty {
                            Some(true)
                        } else if args.no_tty {
                            Some(false)
                        } else {
                            None
                        },
                        no_stdin: args.no_stdin,
                        detach: args.detach,
                        session: args.session,
                        executor: args.executor,
                        output: args.output,
                    },
                )
                .await
            }
            Command::Ps { session } => exec_cli::ps(&endpoint, session).await.map(|_| 0),
            Command::Attach {
                process_id,
                no_replay,
            } => exec_cli::attach(&endpoint, process_id, !no_replay).await,
            Command::Kill { process_id, signal } => {
                let number = prochost::signal_number(&signal)
                    .or_else(|| signal.parse().ok())
                    .unwrap_or(15);
                exec_cli::kill(&endpoint, process_id, number)
                    .await
                    .map(|_| 0)
            }
            Command::Session(command) => session(command, &endpoint).await.map(|_| 0),
            Command::Bench(command) => run_bench(command).map(|_| 0),
            Command::Fs(FsCommand::Cp {
                source,
                destination,
                session,
            }) => fs_cli::cp(&endpoint, &session, &source, &destination)
                .await
                .map(|_| 0),
            Command::Profile(ProfileCommand::Check { profile, commands }) => {
                profile_check(&profile, &commands).map(|_| 0)
            }
            Command::Build {
                recipe,
                out,
                executor,
            } => {
                let out = out.unwrap_or_else(|| needy_home().join("images/default"));
                build::build(recipe.as_deref(), &out, executor).map(|_| 0)
            }
            Command::Report {
                session,
                level,
                output,
            } => {
                let text = report::render(
                    &needy_home().join("trace.db"),
                    &session,
                    matches!(level, ReportLevel::Detailed),
                )?;
                match output {
                    Some(path) => std::fs::write(path, text)?,
                    None => print!("{text}"),
                }
                Ok(0)
            }
        }
    });
    match code {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("needy: {error:#}");
            std::process::exit(1);
        }
    }
}

async fn daemon(args: DaemonArgs, endpoint: &str) -> Result<()> {
    let listen = args.listen.unwrap_or_else(|| endpoint.to_string());
    let token = args
        .token_env
        .as_ref()
        .and_then(|name| std::env::var(name).ok());
    match args.mode {
        Mode::Executor => {
            let service = executor::ExecutorService {
                host: prochost::Host::default(),
                name: args.name,
            };
            let mut routes = tonic::service::Routes::default();
            routes = routes
                .add_service(HealthServiceServer::new(server::Health {
                    instance_id: uuid::Uuid::new_v4().to_string(),
                    role: "executor",
                }))
                .add_service(
                    ProcessServiceServer::new(service)
                        .max_decoding_message_size(usize::MAX)
                        .max_encoding_message_size(usize::MAX),
                );
            eprintln!("needy: executor listening on {listen}");
            server::serve(&listen, routes, token).await
        }
        #[cfg(not(unix))]
        Mode::Control => anyhow::bail!("the control daemon needs a Unix host"),
        #[cfg(unix)]
        Mode::Control => {
            let state = args.state_dir.unwrap_or_else(needy_home);
            let image = args
                .actor_image
                .unwrap_or_else(|| state.join("images/default"));
            let daemon = control::Daemon::start(state, image, args.docker_image).await?;
            eprintln!("needy: control daemon listening on {listen}");
            server::serve(&listen, daemon.routes(), token).await
        }
    }
}

fn run_bench(command: BenchCommand) -> Result<()> {
    match command {
        BenchCommand::Spawn {
            count,
            label,
            command,
        } => bench::spawn(&label, count, &command),
        BenchCommand::Pipe { label, command } => bench::pipe(&label, &command),
        BenchCommand::Fs {
            dir,
            mib,
            files,
            label,
        } => bench::fs(&label, &dir, mib, files),
        BenchCommand::Suite { probes } => {
            for probe in probes {
                let words: Vec<String> = probe.split_whitespace().map(String::from).collect();
                let mut argv = vec!["needy".to_string(), "bench".to_string()];
                argv.extend(words);
                match Cli::try_parse_from(&argv) {
                    Ok(Cli {
                        command: Command::Bench(inner),
                        ..
                    }) => {
                        if let Err(e) = run_bench(inner) {
                            println!("{{\"error\": \"{probe}: {e}\"}}");
                        }
                    }
                    _ => println!("{{\"error\": \"cannot parse probe: {probe}\"}}"),
                }
            }
            Ok(())
        }
    }
}

fn profile_check(path: &std::path::Path, commands: &[String]) -> Result<()> {
    use needy_config::profile::Profile;
    use needy_config::recipe::{HostInfo, Recipe};
    use needy_config::rules::{CommandContext, Target};
    let profile = Profile::load(path)?;
    let base = path.parent().unwrap_or(std::path::Path::new("."));
    println!(
        "user: {} ({}:{})",
        profile.user.name, profile.user.uid, profile.user.gid
    );
    println!(
        "executors: {}",
        profile
            .executors
            .keys()
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!(
        "kernel route names: {}",
        profile
            .route_names()
            .map(|n| n.join(","))
            .unwrap_or_else(|| "* (every exec)".into())
    );
    if let Some(recipe) = &profile.actor.recipe {
        let compiled = Recipe::load(&base.join(recipe))?.compile(&HostInfo::detect()?, 1);
        println!(
            "recipe: {} vCPU, {} MiB, hostname {:?}, uptime offset {} s",
            compiled.vcpus, compiled.mem_mib, compiled.hostname, compiled.uptime_offset_secs
        );
        for (k, v) in &compiled.firecracker_env {
            println!("  {k}={v}");
        }
        for f in &compiled.findings {
            println!("  {:?}: {}", f.severity, f.message);
        }
    }
    for command in commands {
        let argv: Vec<String> = command.split_whitespace().map(String::from).collect();
        let decision = profile.decide(&CommandContext {
            argv: argv.clone(),
            cwd: format!("/home/{}/workspace", profile.user.name),
            env: Default::default(),
            tty: false,
            ancestors: Vec::new(),
        });
        let target = match &decision.target {
            Target::Actor => "actor".to_string(),
            Target::Executor(name) => format!("executor {name}"),
            Target::Deny(message) => format!("DENY ({message})"),
        };
        println!(
            "{command:<40} -> {target:<18} argv={:?} rules={:?}",
            decision.argv, decision.matched
        );
    }
    Ok(())
}

async fn session(command: SessionCommand, endpoint: &str) -> Result<()> {
    use needy_api_proto::v1::{CreateSessionRequest, ListSessionsRequest, StopSessionRequest};
    let client = exec_cli::connect(endpoint).await?;
    let fail = |s: tonic::Status| anyhow::anyhow!("{}", s.message());
    match command {
        SessionCommand::Create {
            profile,
            group,
            name,
            no_actor,
            command,
        } => {
            let yaml = std::fs::read_to_string(&profile)?;
            let dir = std::fs::canonicalize(&profile)?
                .parent()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            let session = client
                .sessions()
                .create_session(CreateSessionRequest {
                    profile_yaml: yaml,
                    profile_dir: dir,
                    group,
                    name,
                    start_actor: !no_actor,
                    actor_command: command,
                })
                .await
                .map_err(fail)?
                .into_inner()
                .session
                .unwrap_or_default();
            println!("{}", session.session_id);
        }
        SessionCommand::Ls => {
            let list = client
                .sessions()
                .list_sessions(ListSessionsRequest {})
                .await
                .map_err(fail)?
                .into_inner();
            println!("{:<13} {:<10} {:<13} EXECUTORS", "ID", "STATE", "GROUP");
            for s in list.sessions {
                let executors: Vec<String> = s
                    .executors
                    .iter()
                    .map(|e| format!("{}={}", e.name, e.runtime))
                    .collect();
                println!(
                    "{:<13} {:<10} {:<13} {}",
                    s.session_id,
                    s.state,
                    s.group,
                    executors.join(",")
                );
            }
        }
        SessionCommand::Stop { session } => {
            client
                .sessions()
                .stop_session(StopSessionRequest {
                    session_id: session,
                })
                .await
                .map_err(fail)?;
        }
    }
    Ok(())
}
