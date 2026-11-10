//! `cargo xtask bench`: latency and throughput probes, run here or inside an
//! actor.
//!
//! The probes run inside an actor, whose image only carries the needy binary,
//! so `bench run` builds this crate statically for the actor and ships it in
//! with the profile's copy-in. The bench profile copies it from
//! `target/bench-probe/`.

mod probe;

use anyhow::Result;
use clap::Subcommand;

#[derive(clap::Args)]
pub struct Args {
  #[command(subcommand)]
  command: Command,
}

#[derive(Subcommand)]
enum Command {
  #[command(flatten)]
  Probe(probe::Probe),
  /// Build the probes for the actor, run a bench profile's run and print
  /// the probes' JSON lines.
  #[cfg(feature = "run")]
  Run(run::Args),
}

pub fn main(args: Args) -> Result<()> {
  match args.command {
    Command::Probe(probe) => probe::run(probe),
    #[cfg(feature = "run")]
    Command::Run(args) => run::run(args),
  }
}

#[cfg(feature = "run")]
mod run {
  use std::path::{Path, PathBuf};
  use std::time::{Duration, Instant};

  use anyhow::{Context, Result, bail};
  use needy_api_client::Client;
  use needy_api_client::proto::v1::{CreateRunRequest, GetRunLogsRequest, GetRunRequest, RunState, StopRunRequest};

  /// The actor's architecture; actors run on x86_64 Linux KVM hosts.
  const PROBE_TARGET: &str = "x86_64-unknown-linux-musl";

  #[derive(clap::Args)]
  pub struct Args {
    /// Bench profile, for example `examples/bench/profile.yaml`.
    #[arg(long)]
    profile: PathBuf,
    /// Control daemon endpoint, as `needy` prints it on start.
    #[arg(long, env = "NEEDY_ENDPOINT")]
    endpoint: String,
    /// Give up when the run has not finished after this many seconds.
    #[arg(long, default_value_t = 1800)]
    timeout_secs: u64,
    /// The daemon's state directory, which holds `trace.db`. With it, and
    /// `trace.timing` on in the profile, the per-hook latencies of the
    /// run follow the probes' lines.
    #[arg(long, env = "NEEDY_STATE_DIR")]
    state_dir: Option<PathBuf>,
  }

  pub fn run(args: Args) -> Result<()> {
    build_probe()?;
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(run_profile(args))
  }

  fn workspace_root() -> PathBuf {
    // NOTICE: xtask always runs through `cargo xtask` from this checkout, so
    // its manifest directory locates the workspace.
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().map(Path::to_path_buf).unwrap_or_default()
  }

  /// Builds the probes without the run feature, so the static binary
  /// needs no TLS or gRPC stack, and stages it for the profile's copy-in.
  fn build_probe() -> Result<()> {
    let root = workspace_root();
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = std::process::Command::new(cargo)
      .current_dir(&root)
      .args([
        "build",
        "--locked",
        "--release",
        "--package",
        "xtask",
        "--no-default-features",
        "--target",
        PROBE_TARGET,
      ])
      .status()
      .context("cannot run cargo")?;
    if !status.success() {
      bail!("cannot build the probes for {PROBE_TARGET}");
    }
    let target = std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from).unwrap_or_else(|| root.join("target"));
    let stage = target.join("bench-probe");
    std::fs::create_dir_all(&stage).with_context(|| format!("cannot create {}", stage.display()))?;
    let built = target.join(PROBE_TARGET).join("release").join("xtask");
    std::fs::copy(&built, stage.join("xtask")).with_context(|| format!("cannot stage {}", built.display()))?;
    Ok(())
  }

  async fn run_profile(args: Args) -> Result<()> {
    let mut client = Client::connect(&args.endpoint).await.with_context(|| format!("cannot reach the daemon at {}", args.endpoint))?;
    if let Ok(token) = std::env::var("NEEDY_TOKEN")
      && !token.is_empty()
    {
      client = client.with_token(&token)?;
    }
    let path = std::fs::canonicalize(&args.profile).with_context(|| format!("cannot read profile {}", args.profile.display()))?;
    // As `needy run create` does: the daemon reads no client path.
    let resolved = needy_config::profile::resolve(&needy_config::Source::file(&path))?;
    let created = client
      .runs()
      .create_run(CreateRunRequest {
        profile_yaml: resolved.yaml,
        recipe_yaml: resolved.recipe_yaml.unwrap_or_default(),
        profile_name: needy_config::profile::name(&path),
        ..Default::default()
      })
      .await
      .map_err(|s| anyhow::anyhow!("cannot create the run: {}", s.message()))?
      .into_inner()
      .run
      .unwrap_or_default();
    let id = created.run_id;
    println!("{}", serde_json::json!({"run": id}));
    let deadline = Instant::now() + Duration::from_secs(args.timeout_secs);
    loop {
      let state = client
        .runs()
        .get_run(GetRunRequest { run_id: id.clone() })
        .await
        .map_err(|s| anyhow::anyhow!("run {id}: {}", s.message()))?
        .into_inner()
        .run
        .map(|s| s.state())
        .unwrap_or_default();
      if state != RunState::Running {
        break;
      }
      if Instant::now() > deadline {
        bail!("run {id} did not finish in {} s", args.timeout_secs);
      }
      tokio::time::sleep(Duration::from_secs(1)).await;
    }
    let mut logs = client
      .runs()
      .get_run_logs(GetRunLogsRequest {
        run_id: id.clone(),
        follow: false,
      })
      .await
      .map_err(|s| anyhow::anyhow!("run {id} logs: {}", s.message()))?
      .into_inner();
    let mut console = Vec::new();
    while let Some(chunk) = logs.message().await.map_err(|s| anyhow::anyhow!("run {id} logs: {}", s.message()))? {
      console.extend_from_slice(&chunk.data);
    }
    // The console also carries the guest's boot output; the probes' results
    // are the lines that are JSON objects.
    for line in String::from_utf8_lossy(&console).lines().map(str::trim) {
      if line.starts_with('{') && serde_json::from_str::<serde_json::Value>(line).is_ok() {
        println!("{line}");
      }
    }
    // Stopping the run writes its timing summary to the trace.
    client
      .runs()
      .stop_run(StopRunRequest { run_id: id.clone() })
      .await
      .map_err(|s| anyhow::anyhow!("cannot stop run {id}: {}", s.message()))?;
    match &args.state_dir {
      Some(dir) => print_timing(&dir.join("trace.db"), &id).await?,
      None => eprintln!("pass --state-dir or NEEDY_STATE_DIR to print per-hook latencies"),
    }
    Ok(())
  }

  /// Prints each `trace.timing` event of `run` as `{"timing": {...}}`.
  async fn print_timing(db: &Path, run: &str) -> Result<()> {
    let loaded = needy_trace::sink::sqlite::load(db, run).await.with_context(|| format!("cannot read the trace of {run}"))?;
    let mut any = false;
    for event in loaded.events.iter().filter(|e| e.kind == needy_trace::event::TIMING) {
      any = true;
      println!("{{\"timing\":{}}}", event.payload.get());
    }
    if !any {
      eprintln!("run {run} has no per-hook latencies; set `trace: {{ timing: true }}` in the profile");
    }
    Ok(())
  }
}
