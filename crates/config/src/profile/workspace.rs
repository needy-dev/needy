//! The actor's workspace, written once as `actor.workspace`, turned into
//! what the rest of a profile already knows: a file mapping that makes it
//! appear in the actor and the path map that sends routed commands to the
//! same files on the executor. Before 2026-11-12 a profile wrote all three
//! (`actor.cwd`, a `files` mount and `path_map`), and they had to agree.

use super::files::{FileMapping, FileMode, MirrorWhen, Side};
use super::{ExecutorDriver, Profile};
use crate::{Error, Kind, Source};

/// The name of the file mapping an `actor.workspace` derives: a mount from
/// an executor, or a copy-in from the host.
pub const WORKSPACE: &str = "workspace";
/// The name of the mirror-out a writable `host:` workspace derives.
pub const WORKSPACE_OUT: &str = "workspace-out";

/// Derives the file mappings and the path map of `profile`'s
/// `actor.workspace`, after its `extends` chain merged and its actor paths
/// expanded `~`. A workspace that contradicts what the profile writes itself
/// is an error, not a silent override.
// NOTICE: these are load errors rather than `validate` findings: the
// derived profile must be consistent before anything reads it, and a
// finding about `files.workspace` would name a mapping the user never
// wrote.
pub(super) fn derive(profile: &mut Profile, source: &Source) -> Result<(), Error> {
  let Some(workspace) = profile.actor.workspace.as_mut() else {
    return Ok(());
  };
  let invalid = |path: &str, message: String| Error::Invalid {
    kind: Kind::Profile,
    origin: source.to_string(),
    path: path.into(),
    message,
  };
  let path = profile.user.expand_home(&workspace.path);
  if !path.starts_with('/') {
    return Err(invalid("actor.workspace.path", format!("{path:?} must be absolute or start with `~`")));
  }
  let path = trim(&path);
  workspace.path = path.clone();
  let home = profile.user.home();
  let in_home = path == home || path.starts_with(&format!("{home}/"));
  let workspace = workspace.clone();
  let mut derived = vec![WORKSPACE];
  match &workspace.from {
    Some(Side::Actor(_)) => {
      return Err(invalid(
        "actor.workspace.from",
        "the workspace comes from `executor:NAME:PATH` or `host:PATH`; leave `from` out for a fresh directory in the actor".into(),
      ));
    }
    // NOTICE: the actor's root file system is read-only and only its home
    // is a fresh tmpfs, so a directory nothing mounts must be under it.
    None | Some(Side::Host(_)) if !in_home => {
      return Err(invalid(
        "actor.workspace.path",
        format!("{path} is outside the user's home {home}; without an executor mount the workspace is a directory in the actor's home"),
      ));
    }
    None => return Ok(()),
    Some(Side::Executor {
      name,
      path: on_executor,
    }) => {
      let Some(executor) = profile.executors.get_mut(name) else {
        return Err(invalid("actor.workspace.from", format!("names unknown executor {name:?}")));
      };
      let on_executor = trim(on_executor);
      if let Some(written) = executor.path_map.get(&path) {
        return Err(invalid(
          &format!("executors.{name}.path_map"),
          format!(
            "maps {path} to {written}, which actor.workspace already maps to {on_executor}; remove the path_map entry or the workspace"
          ),
        ));
      }
      executor.path_map.insert(path.clone(), on_executor.clone());
      // `needy exec` without a directory starts in the workspace. A
      // `linux-container` executor's `workspace` is the host directory its
      // container mounts, not a path in the container, so it is left alone.
      if executor.driver == ExecutorDriver::Native && executor.workspace.is_none() {
        executor.workspace = Some(on_executor.clone());
      }
      insert(
        profile,
        source,
        WORKSPACE,
        FileMapping {
          mode: FileMode::Mount,
          from: Side::Executor {
            name: name.clone(),
            path: on_executor,
          },
          to: Side::Actor(path),
          filter: Vec::new(),
          writable: workspace.writable,
          when: MirrorWhen::End,
          delete: false,
        },
      )?;
    }
    Some(Side::Host(on_host)) => {
      insert(
        profile,
        source,
        WORKSPACE,
        FileMapping {
          mode: FileMode::CopyIn,
          from: Side::Host(on_host.clone()),
          to: Side::Actor(path.clone()),
          filter: Vec::new(),
          writable: false,
          when: MirrorWhen::End,
          delete: false,
        },
      )?;
      if workspace.writable {
        derived.push(WORKSPACE_OUT);
        insert(
          profile,
          source,
          WORKSPACE_OUT,
          FileMapping {
            mode: FileMode::MirrorOut,
            from: Side::Actor(path),
            to: Side::Host(on_host.clone()),
            filter: Vec::new(),
            writable: false,
            when: MirrorWhen::End,
            delete: false,
          },
        )?;
      }
    }
  }
  // An empty `run_files` selects every mapping already; a list that does
  // not name the workspace still gets it.
  if !profile.run_files.is_empty() {
    for name in derived {
      if !profile.run_files.iter().any(|n| n == name) {
        profile.run_files.push(name.into());
      }
    }
  }
  Ok(())
}

/// Adds a derived mapping, refusing a mapping of that name the profile
/// writes itself.
fn insert(profile: &mut Profile, source: &Source, name: &str, mapping: FileMapping) -> Result<(), Error> {
  if profile.files.mappings.contains_key(name) {
    return Err(Error::Invalid {
      kind: Kind::Profile,
      origin: source.to_string(),
      path: format!("files.{name}"),
      message: "actor.workspace defines this mapping; move the workspace's settings to actor.workspace or rename the mapping".into(),
    });
  }
  profile.files.mappings.insert(name.into(), mapping);
  Ok(())
}

/// `path` without trailing slashes, except the root.
fn trim(path: &str) -> String {
  let trimmed = path.trim_end_matches('/');
  if trimmed.is_empty() {
    path.into()
  } else {
    trimmed.into()
  }
}

#[cfg(test)]
#[path = "workspace_test.rs"]
mod tests;
