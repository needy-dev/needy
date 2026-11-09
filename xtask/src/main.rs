//! `cargo xtask <task>`: developer tasks that are not part of the product
//! binary. Today that is `bench`.

mod bench;

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
}

fn main() -> Result<()> {
  match Cli::parse().task {
    Task::Bench(args) => bench::main(args),
  }
}
