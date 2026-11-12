//! Which run a command addresses, so a run ID need not be copied by hand
//! (owner decision, 2026-11-12). A command's `--run` (or `RUN` argument)
//! wins, else `NEEDY_RUN`; either may be a full ID, an ID prefix, a run's
//! name or `latest`, the run created last. Commands that only make sense
//! about a run (`run attach`, `run stop`, `run logs`) then take the only
//! live run; `exec`, `ps` and `fs cp`, which mean something without a run,
//! stay outside any run instead (the CLI UX slice's choice, which keeps
//! `needy exec -- CMD` from changing meaning when a run starts).
//!
//! The run is found among the control plane's runs (`ListRuns`) and sent as
//! its full ID, so every command reads a prefix the same way, also `ps`,
//! whose executors match run IDs exactly.

use anyhow::{Result, bail};
use needy_api_client::Client;
use needy_api_client::proto::v1::{self, ListRunsRequest, Run};

/// The variable that names the run when a command does not.
pub const ENV: &str = "NEEDY_RUN";

/// The full ID of the run `given` or `NEEDY_RUN` names, else of the only
/// live run. An empty `given` counts as not given.
pub async fn select(connection: &Client, given: Option<String>) -> Result<String> {
  let runs = list(connection).await?;
  match wanted(given.filter(|g| !g.is_empty())) {
    Some(wanted) => find(&runs, &wanted),
    None => only_live(&runs),
  }
}

/// The full ID of the run `given` or `NEEDY_RUN` names, or `None` outside any
/// run: when neither names one, or `given` is empty (`--run ''`), which
/// leaves a run `NEEDY_RUN` names.
pub async fn select_or_none(connection: &Client, given: Option<String>) -> Result<Option<String>> {
  match wanted(given) {
    Some(wanted) if !wanted.is_empty() => Ok(Some(find(&list(connection).await?, &wanted)?)),
    _ => Ok(None),
  }
}

/// `given`, else `NEEDY_RUN` when it is set and not empty.
pub fn wanted(given: Option<String>) -> Option<String> {
  given.or_else(|| std::env::var(ENV).ok().filter(|v| !v.is_empty()))
}

async fn list(connection: &Client) -> Result<Vec<Run>> {
  let request = ListRunsRequest {};
  Ok(connection.runs().list_runs(request).await.map_err(needy_api_client::Error::from)?.into_inner().runs)
}

/// The ID of the one run `wanted` names: `latest`, an exact ID, else the
/// only run whose ID starts with `wanted` or whose name is `wanted`.
fn find(runs: &[Run], wanted: &str) -> Result<String> {
  if wanted == "latest" {
    // `max_by_key` keeps the last of equals, and runs come oldest first.
    return match runs.iter().max_by_key(|run| run.created_at_ms) {
      Some(run) => Ok(run.run_id.clone()),
      None => bail!("there is no run yet; create one with `needy run create`"),
    };
  }
  if let Some(exact) = runs.iter().find(|run| run.run_id == wanted) {
    return Ok(exact.run_id.clone());
  }
  let matches: Vec<&Run> = runs.iter().filter(|run| run.run_id.starts_with(wanted) || run.name == wanted).collect();
  match matches.as_slice() {
    [] => bail!("no run {wanted}"),
    [one] => Ok(one.run_id.clone()),
    several => bail!("run {wanted} is ambiguous: {} runs match; give more of the ID:\n{}", several.len(), table(several)),
  }
}

/// The ID of the only run being created or running.
fn only_live(runs: &[Run]) -> Result<String> {
  let live: Vec<&Run> = runs.iter().filter(|run| is_live(run)).collect();
  match live.as_slice() {
    [] => bail!("no run is live; name a run (an ID, an ID prefix, a name or `latest`) or create one with `needy run create`"),
    [one] => Ok(one.run_id.clone()),
    several => {
      bail!("{} runs are live; name one (an ID, an ID prefix, a name or `latest`), or set {ENV}:\n{}", several.len(), table(several))
    }
  }
}

fn is_live(run: &Run) -> bool {
  matches!(v1::RunState::try_from(run.state), Ok(v1::RunState::Creating | v1::RunState::Running))
}

/// The runs as an indented table with their ID, profile and age.
fn table(runs: &[&Run]) -> String {
  let rows: Vec<[String; 3]> = std::iter::once(["ID".into(), "PROFILE".into(), "AGE".into()])
    .chain(runs.iter().map(|run| {
      [
        run.run_id.clone(),
        run.profile.clone(),
        crate::commands::runner::ago(run.created_at_ms),
      ]
    }))
    .collect();
  let width = |column: usize| rows.iter().map(|row| row[column].chars().count()).max().unwrap_or(0);
  let (id, profile) = (width(0), width(1));
  rows.iter().map(|[i, p, a]| format!("  {i:<id$}  {p:<profile$}  {a}")).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
#[path = "runs_test.rs"]
mod tests;
