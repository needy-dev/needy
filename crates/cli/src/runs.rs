//! Resolve run references through the selected control plane. Run operations
//! may inherit a fixed execution reference; process creation never calls this.

use anyhow::Result;
use needy_api_client::Client;
use needy_api_client::proto::v1::{self, ListRunsRequest, Run};

/// Resolve an explicit/inherited reference, otherwise require one live run.
pub async fn select(connection: &Client, given: Option<String>, ctx: &crate::cli::Context) -> Result<String> {
  let runs = list(connection).await?;
  match wanted(given, ctx)? {
    Some(wanted) => find(&runs, &wanted),
    None => only_live(&runs),
  }
}

/// A caller's explicit run wins. An explicit target suppresses inherited run
/// selection, so a reference from another control plane cannot redirect it.
pub fn wanted(given: Option<String>, ctx: &crate::cli::Context) -> Result<Option<String>> {
  if let Some(given) = given {
    anyhow::ensure!(!given.is_empty(), "RUN cannot be empty; choose a run explicitly");
    return Ok(Some(given));
  }
  if ctx.selected_context.is_some() || ctx.address.is_some() {
    return Ok(None);
  }
  if let Some(execution) = crate::connection::inherited()? {
    return Ok(execution.run);
  }
  Ok(None)
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
      None => Err(
        crate::diagnostics::Error::new("run.not_found", "There is no run yet")
          .check("Create one with `needy run create --profile PROFILE`.")
          .into(),
      ),
    };
  }
  if let Some(exact) = runs.iter().find(|run| run.run_id == wanted) {
    return Ok(exact.run_id.clone());
  }
  let matches: Vec<&Run> = runs.iter().filter(|run| run.run_id.starts_with(wanted) || run.name == wanted).collect();
  match matches.as_slice() {
    [] => Err(
      crate::diagnostics::Error::new("run.not_found", format!("No run {wanted}"))
        .check("List runs on this control plane with `needy run ls`.")
        .into(),
    ),
    [one] => Ok(one.run_id.clone()),
    several => Err(
      crate::diagnostics::Error::new(
        "run.ambiguous",
        format!("run {wanted} is ambiguous: {} runs match; give more of the ID:\n{}", several.len(), table(several)),
      )
      .check("Give a full run ID from `needy run ls`.")
      .into(),
    ),
  }
}

/// The ID of the only run being created or running.
fn only_live(runs: &[Run]) -> Result<String> {
  let live: Vec<&Run> = runs.iter().filter(|run| is_live(run)).collect();
  match live.as_slice() {
    [] => Err(
      crate::diagnostics::Error::new(
        "run.not_found",
        "no run is live; name a run (an ID, a unique prefix, a name or latest) or create one with `needy run create`",
      )
      .into(),
    ),
    [one] => Ok(one.run_id.clone()),
    several => Err(
      crate::diagnostics::Error::new(
        "run.ambiguous",
        format!("{} runs are live; name one (an ID, a unique prefix, a name or latest):\n{}", several.len(), table(several)),
      )
      .into(),
    ),
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
