//! `cargo xtask <task>`: developer tasks that are not part of the product
//! binary: `bench`, and `tui`, the terminal fidelity harness.

mod bench;
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
}

fn main() -> Result<()> {
  match Cli::parse().task {
    Task::Bench(args) => bench::main(args),
    #[cfg(all(unix, feature = "tui"))]
    Task::Tui(args) => tui::main(args),
  }
}
