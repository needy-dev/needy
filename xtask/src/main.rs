//! `cargo xtask <task>`: developer tasks that are not part of the product
//! binary: `bench`, `tui`, the terminal fidelity harness, `e2e`, the
//! end-to-end matrix, and `mock-model`, a scripted model server for agents.

mod bench;
#[cfg(feature = "run")]
mod e2e;
#[cfg(feature = "mock-model")]
mod mock_model;
// NOTICE: the harness drives Unix pseudo-terminals; Windows terminals are
// not part of the TUI matrix (see the terminal note).
#[cfg(all(unix, feature = "tui"))]
mod tui;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
  name = "cargo xtask",
  about = "Developer tasks for the needy workspace"
)]
struct Cli {
  #[command(subcommand)]
  task: Task,
}

#[derive(Subcommand)]
enum Task {
  /// Latency and throughput probes, alone or inside an actor.
  Bench(bench::Args),
  /// Drive a program on a pseudo-terminal from a scenario and record what a
  /// terminal shows: the TUI fidelity harness.
  #[cfg(all(unix, feature = "tui"))]
  Tui(tui::Args),
  /// End-to-end matrix: every actor driver this host has, with its terminal
  /// modes, run endings, file mappings, chaos, network and users, each cell
  /// against its own non-root control daemon.
  #[cfg(feature = "run")]
  E2e(e2e::Args),
  /// A scripted OpenAI-compatible model server: it answers a coding agent
  /// with one shell tool call, then with the call's output, so the agent's
  /// loop runs without an account.
  #[cfg(feature = "mock-model")]
  MockModel(mock_model::Args),
}

fn main() -> Result<()> {
  match Cli::parse().task {
    Task::Bench(args) => bench::main(args),
    #[cfg(all(unix, feature = "tui"))]
    Task::Tui(args) => tui::main(args),
    #[cfg(feature = "run")]
    Task::E2e(args) => {
      let code = e2e::main(args)?;
      std::process::exit(code);
    }
    #[cfg(feature = "mock-model")]
    Task::MockModel(args) => mock_model::main(args),
  }
}
