//! Output formats for people (borderless tables) and tools (JSON). Rows are
//! typed structs; each command defines its own.

mod table;

use anyhow::Result;
use serde::Serialize;

#[derive(Clone, Copy, Debug, Default, clap::ValueEnum)]
pub enum OutputFormat {
  #[default]
  Table,
  Json,
}

/// A row of a table: its header and its cells, in the same order.
pub trait Row: Serialize {
  const HEADER: &'static [&'static str];
  fn cells(&self) -> Vec<String>;
}

/// Prints rows as a table, or as a JSON array of objects.
pub fn print<R: Row>(format: OutputFormat, rows: &[R]) -> Result<()> {
  match format {
    OutputFormat::Table => print!("{}", table::render(R::HEADER, rows.iter().map(Row::cells))),
    OutputFormat::Json => println!("{}", serde_json::to_string_pretty(rows)?),
  }
  Ok(())
}

/// Prints a table of rows without a JSON form, for sections of a text report.
pub fn print_table<R: Row>(rows: &[R]) {
  print!("{}", table::render(R::HEADER, rows.iter().map(Row::cells)));
}

/// Prints a message for people on stderr, such as a failure the command
/// handled itself or a warning, with the prefix of an error that ends a command.
pub fn message(message: impl std::fmt::Display) {
  eprintln!("needy: {message}");
}
