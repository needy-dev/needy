//! The default workspace: with no `actor.workspace`, a run works live in
//! the folder `needy run create` runs in, when the executor that runs its
//! commands can reach that folder (the images and run data spec,
//! `docs/ai/references/architecture/2026-11-17-images-and-run-data-spec.md`,
//! "Workspace default").

use anyhow::{Result, bail};
use needy_api_server::control::RunWorkspace;
use needy_config::Source;
use needy_config::profile::{ExecutorDriver, Profile, RunnerSelector, Side, Workspace};

/// Where the default workspace appears in the actor.
const PATH: &str = "~/workspace";

/// Makes `folder`, on the machine of the client that creates the run, the
/// workspace of `profile` when it names none, through the executor that
/// runs its commands: `default_executor`, else its only executor.
/// `on_control_host` says whether that client runs on the control host.
///
/// The folder is reachable from a native executor of the creating client
/// (its execution holder runs there), and from a native or
/// `linux-container` executor on the control host when the client runs on
/// it too; a `linux-container` executor binds it as its workspace. Any
/// other executor fails the create, asking for `workspace.from`.
// NOTICE: there is no guard for the home directory or `/` (owner decision,
// 2026-11-17, in the spec's "Changes from the designs").
// NOTICE: a profile without executors keeps the actor's home, as before:
// the actor runs every command itself, and nothing can pass the folder
// through to it, so no executor is "on the wrong machine" either (user
// story 57 fails only commands that run on a remote executor).
// NOTICE: a runner is taken to be another machine, even when it runs on
// the control host or the client's machine: no fact the control plane
// keeps says which machine a runner is on.
// TODO(workspaces): a managed workspace for a create without a folder, or
// from a folder no executor reaches, is deferred with managed workspaces
// (the spec's "Out of Scope").
pub(crate) fn default(profile: &mut Profile, folder: &str, on_control_host: bool, source: &Source) -> Result<()> {
  if profile.actor.workspace.is_some() || folder.is_empty() {
    return Ok(());
  }
  let name = match (&profile.default_executor, profile.executors.len()) {
    (Some(name), _) => name.clone(),
    (None, 0) => return Ok(()),
    (None, 1) => profile.executors.keys().next().cloned().unwrap_or_default(),
    (None, _) => bail!(
      "the workspace defaults to the folder `needy run create` runs in ({folder}), but the profile has several executors and no \
       default_executor to pass it through; set default_executor, or set actor.workspace with `from: executor:NAME:PATH` (workspace.from)"
    ),
  };
  // `validate` reports a `default_executor` that names no executor.
  let Some(executor) = profile.executors.get_mut(&name) else {
    return Ok(());
  };
  let ask = |reason: String| {
    anyhow::anyhow!(
      "the workspace defaults to the folder `needy run create` runs in ({folder}), but executor {name} {reason}; set actor.workspace \
       with `from: executor:{name}:PATH` and a path on that machine (workspace.from)"
    )
  };
  let on_executor = match (&executor.runner, executor.driver) {
    (Some(RunnerSelector::Client), ExecutorDriver::Native) => folder.to_string(),
    (Some(RunnerSelector::Client), driver) => {
      return Err(ask(format!("is a {driver} executor of the creating client, which needy cannot create")));
    }
    (Some(runner), _) => return Err(ask(format!("runs on {runner}, another machine"))),
    (None, _) if !on_control_host => return Err(ask("runs on the control host, another machine than the client's".into())),
    (None, ExecutorDriver::Native) => folder.to_string(),
    (None, ExecutorDriver::LinuxContainer) => {
      if let Some(bound) = &executor.workspace {
        return Err(ask(format!("binds its own workspace {bound}")));
      }
      executor.workspace = Some(folder.to_string());
      // The container sees the directory it binds at `~/workspace`
      // (`needy_core_executor::driver::linux_container::options`); the
      // daemon expands this `~` to the container's home.
      PATH.to_string()
    }
  };
  let workspace = Workspace {
    path: PATH.into(),
    from: Some(Side::Executor {
      name,
      path: on_executor,
    }),
    writable: true,
  };
  needy_config::profile::set_workspace(profile, workspace, source)
    .map_err(|error| anyhow::anyhow!("cannot make the folder `needy run create` runs in ({folder}) the workspace: {error}"))
}

/// Where `profile`'s workspace is, for the run's output.
pub(crate) fn describe(profile: &Profile) -> Option<RunWorkspace> {
  let workspace = profile.actor.workspace.as_ref()?;
  let (executor, source) = match &workspace.from {
    Some(Side::Executor { name, path }) => {
      // A `linux-container` executor shows the host directory it binds, not
      // the path its container sees.
      let bound = profile.executors.get(name).filter(|e| e.driver == ExecutorDriver::LinuxContainer).and_then(|e| e.workspace.clone());
      (name.clone(), bound.unwrap_or_else(|| path.clone()))
    }
    Some(Side::Host(path)) => (String::new(), path.clone()),
    Some(Side::Actor(_)) | None => (String::new(), String::new()),
  };
  Some(RunWorkspace {
    path: workspace.path.clone(),
    executor,
    source,
    writable: workspace.writable,
  })
}

/// Warns, and says so, when `mounts`, where run `id`'s actor mounts
/// `profile`'s workspace on the control host, is inside that workspace's
/// folder on the control host: the mount then contains itself, and a walk
/// of the workspace does not end. That is the case where the runtime
/// directory falls back to the state directory and the run is created in a
/// folder that contains it, such as `$HOME`
/// (`Daemon::run_mounts_dir`).
// TODO(workspaces): a warning, not a refusal; refusing, or a mount root
// that never falls back into the user's tree, needs the owner's decision.
pub(crate) fn warn_contains_mounts(id: &str, profile: &Profile, mounts: &std::path::Path) -> bool {
  let Some(workspace) = describe(profile) else {
    return false;
  };
  // A workspace on a runner is on another machine than these mounts.
  let on_control_host = profile.executors.get(&workspace.executor).is_some_and(|e| e.runner.is_none());
  if !on_control_host || workspace.source.is_empty() || !mounts.starts_with(&workspace.source) {
    return false;
  }
  tracing::warn!(
    "run {id}: its workspace mount points {} are inside its workspace {}, so the mount contains itself and a walk of the \
     workspace does not end; set XDG_RUNTIME_DIR or NEEDY_RUNTIME_DIR to a directory outside the workspace",
    mounts.display(),
    workspace.source
  );
  true
}

#[cfg(test)]
#[path = "workspace_test.rs"]
mod tests;
