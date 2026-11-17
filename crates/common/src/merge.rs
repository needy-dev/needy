//! Three-way merge of a directory tree: a run's copy of a tree merged back
//! into the store it was seeded from, against that seed, entry by entry.
//!
//! The merge touches only the directories it is given. It knows no file
//! format: entries are compared by content, and the one text-aware choice,
//! [`Choice::Lines`], treats a file as append-only bytes.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::filter::{filter_allows, matches};

/// The three trees of a merge: the store as the run was seeded from it, the
/// run's copy at its end, and the store as it is now. A missing directory is
/// an empty tree; a missing store is created.
#[derive(Debug, Clone, Copy)]
pub struct Trees<'a> {
  pub seed: &'a Path,
  pub run: &'a Path,
  pub store: &'a Path,
}

/// What resolves an entry that both the run and the store changed since the
/// seed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
  /// The store's copy stays; the run's is dropped without a conflict.
  Store,
  /// The run's copy replaces the store's.
  Run,
  /// An append-only text file: the bytes the run appended to the seed's
  /// copy are appended to the store's. A run that did more than append is a
  /// conflict.
  Lines,
  // TODO(merge-drivers): a choice that hands every file its glob matches
  // to a merge driver as three trees is slice M2 of
  // 2026-11-17-images-and-run-data-spec.md; it plugs in here, and the merge
  // then skips the files a driver claims.
}

/// How to merge a tree.
#[derive(Debug, Clone, Default)]
pub struct Policy {
  /// rclone-style filter rules ([`crate::filter`]): an excluded entry is not
  /// merged, in any of the three trees.
  pub filter: Vec<String>,
  /// Choices by pattern (as a filter rule's pattern matches), in order: the
  /// first pattern that matches an entry's relative path applies.
  pub on_conflict: Vec<(String, Choice)>,
}

/// What the merge did with one entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
  /// Only the run changed or added it; the store now has the run's copy.
  Taken,
  /// Only the run removed it; it is removed from the store.
  Removed,
  /// Both changed it; the policy's choice resolved it.
  Resolved(Choice),
  /// Both changed it and nothing resolved it: the store's copy stays, and
  /// the run's (when the run kept one) is under the merge's aside directory
  /// at the same relative path.
  Conflict,
}

/// One entry the merge acted on, by its path relative to the tree's root
/// with `/` separators.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
  pub path: String,
  pub action: Action,
}

/// Every entry the merge acted on, in path order. Entries the run left as
/// the seed had them, or changed exactly as the store did, are not listed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outcome {
  pub changes: Vec<Change>,
}

impl Outcome {
  /// The paths left in conflict, whose run copies are aside.
  pub fn conflicts(&self) -> impl Iterator<Item = &str> {
    self.changes.iter().filter(|c| c.action == Action::Conflict).map(|c| c.path.as_str())
  }
}

/// Merges `trees.run` into `trees.store` against `trees.seed`, entry by
/// entry:
///
/// - an entry the run did not change keeps the store's copy, however the
///   store changed;
/// - an entry only the run changed, added or removed is taken from the run;
/// - an entry both changed follows the first matching `on_conflict` choice,
///   and without one keeps the store's copy and copies the run's under
///   `aside`, reported as a conflict.
///
/// Entries are files, symbolic links and directories, compared by content
/// (a link by its target, a directory by being one). Files are replaced by
/// a rename, so an interrupted merge leaves each file whole, and running it
/// again finishes it: what was taken now equals the run's copy, and lines
/// already appended are not appended twice.
// NOTICE: permissions and times are not compared: a change of mode alone is
// not a change. A taken file keeps the run's permissions.
pub fn merge(trees: Trees<'_>, aside: &Path, policy: &Policy) -> Result<Outcome> {
  let seed = scan(trees.seed, &policy.filter)?;
  let run = scan(trees.run, &policy.filter)?;
  let store = scan(trees.store, &policy.filter)?;
  let paths: BTreeSet<&String> = seed.keys().chain(run.keys()).chain(store.keys()).collect();

  let mut plan = Vec::new();
  for path in paths {
    let (s, r, t) = (seed.get(path), run.get(path), store.get(path));
    if same(s, r)? || same(t, r)? {
      continue;
    }
    let step = if same(t, s)? {
      Step::Take
    } else {
      match choice(policy, path) {
        Some(Choice::Store) => Step::Keep,
        Some(Choice::Run) => Step::TakeChosen,
        Some(Choice::Lines) => Step::Lines,
        None => Step::Aside,
      }
    };
    plan.push((path.clone(), step));
  }

  std::fs::create_dir_all(trees.store).with_context(|| format!("create store {}", trees.store.display()))?;
  let mut done: BTreeMap<String, Action> = BTreeMap::new();

  // Removals first, deepest first, so a directory is empty by the time it is
  // removed and a path is free before something of another kind lands on it.
  for (path, step) in plan.iter().rev() {
    let taking = matches!(step, Step::Take | Step::TakeChosen);
    if !taking || run.contains_key(path) {
      continue;
    }
    if remove(&trees.store.join(path))? {
      let action = if *step == Step::Take {
        Action::Removed
      } else {
        Action::Resolved(Choice::Run)
      };
      done.insert(path.clone(), action);
    }
    // A directory the store still has entries in stays; the entries the
    // store added since the seed are its own.
  }

  for (path, step) in &plan {
    let target = trees.store.join(path);
    let action = match step {
      Step::Keep => Action::Resolved(Choice::Store),
      Step::Aside => {
        keep_aside(&run, path, aside)?;
        Action::Conflict
      }
      Step::Lines => match appended(seed.get(path), run.get(path), store.get(path))? {
        Some(bytes) => {
          append(&target, &bytes)?;
          Action::Resolved(Choice::Lines)
        }
        None => {
          keep_aside(&run, path, aside)?;
          Action::Conflict
        }
      },
      Step::Take | Step::TakeChosen => {
        let Some(node) = run.get(path) else {
          continue;
        };
        if !place(trees.store, path, node)? {
          keep_aside(&run, path, aside)?;
          Action::Conflict
        } else if *step == Step::Take {
          Action::Taken
        } else {
          Action::Resolved(Choice::Run)
        }
      }
    };
    done.insert(path.clone(), action);
  }

  Ok(Outcome {
    changes: done.into_iter().map(|(path, action)| Change { path, action }).collect(),
  })
}

/// What the merge does with one entry, before it touches the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
  /// Only the run changed it.
  Take,
  /// Both changed it; `Choice::Run`.
  TakeChosen,
  /// Both changed it; `Choice::Store`.
  Keep,
  /// Both changed it; `Choice::Lines`.
  Lines,
  /// Both changed it and no choice applies.
  Aside,
}

fn choice(policy: &Policy, path: &str) -> Option<Choice> {
  policy.on_conflict.iter().find(|(pattern, _)| matches(pattern, path)).map(|(_, choice)| *choice)
}

/// One entry of a tree, at `path` on disk.
#[derive(Debug, Clone)]
struct Node {
  path: PathBuf,
  kind: Kind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Kind {
  File,
  Link(PathBuf),
  Dir,
}

/// Every entry under `root` that `filter` allows, by relative path. A
/// directory the filter excludes is skipped with its contents, as
/// [`crate::archive::pack`] skips it.
fn scan(root: &Path, filter: &[String]) -> Result<BTreeMap<String, Node>> {
  let mut nodes = BTreeMap::new();
  if !root.is_dir() {
    return Ok(nodes);
  }
  let walker = walkdir::WalkDir::new(root).min_depth(1).into_iter().filter_entry(|entry| {
    let relative = relative(root, entry.path());
    let probe = if entry.file_type().is_dir() {
      format!("{relative}/")
    } else {
      relative
    };
    filter_allows(filter, &probe)
  });
  for entry in walker {
    let entry = entry.with_context(|| format!("scan {}", root.display()))?;
    let file_type = entry.file_type();
    let kind = if file_type.is_dir() {
      Kind::Dir
    } else if file_type.is_symlink() {
      Kind::Link(std::fs::read_link(entry.path()).with_context(|| format!("read link {}", entry.path().display()))?)
    } else {
      Kind::File
    };
    nodes.insert(
      relative(root, entry.path()),
      Node {
        path: entry.path().to_path_buf(),
        kind,
      },
    );
  }
  Ok(nodes)
}

fn relative(root: &Path, path: &Path) -> String {
  path.strip_prefix(root).unwrap_or(path).to_string_lossy().replace('\\', "/")
}

/// Whether two entries are the same: both absent, or of one kind with the
/// same content.
fn same(a: Option<&Node>, b: Option<&Node>) -> Result<bool> {
  match (a, b) {
    (None, None) => Ok(true),
    (Some(a), Some(b)) => match (&a.kind, &b.kind) {
      (Kind::File, Kind::File) => same_content(&a.path, &b.path),
      (a, b) => Ok(a == b),
    },
    _ => Ok(false),
  }
}

fn same_content(a: &Path, b: &Path) -> Result<bool> {
  let (ma, mb) = (std::fs::metadata(a)?, std::fs::metadata(b)?);
  if ma.len() != mb.len() {
    return Ok(false);
  }
  let open = |p: &Path| std::fs::File::open(p).with_context(|| format!("open {}", p.display()));
  let (mut fa, mut fb) = (open(a)?, open(b)?);
  let (mut ba, mut bb) = (vec![0u8; 64 * 1024], vec![0u8; 64 * 1024]);
  loop {
    let na = read_full(&mut fa, &mut ba)?;
    let nb = read_full(&mut fb, &mut bb)?;
    if na != nb || ba[..na] != bb[..nb] {
      return Ok(false);
    }
    if na == 0 {
      return Ok(true);
    }
  }
}

/// Reads until `buf` is full or the file ends.
fn read_full(file: &mut std::fs::File, buf: &mut [u8]) -> Result<usize> {
  let mut n = 0;
  while n < buf.len() {
    match file.read(&mut buf[n..])? {
      0 => break,
      read => n += read,
    }
  }
  Ok(n)
}

/// Removes the entry at `path`, a directory only when it is empty. Returns
/// whether it is gone.
fn remove(path: &Path) -> Result<bool> {
  let meta = match std::fs::symlink_metadata(path) {
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(true),
    meta => meta?,
  };
  if meta.is_dir() {
    if std::fs::read_dir(path)?.next().is_some() {
      return Ok(false);
    }
    std::fs::remove_dir(path)
  } else {
    std::fs::remove_file(path)
  }
  .with_context(|| format!("remove {}", path.display()))?;
  Ok(true)
}

/// Puts the run's `node` at `path` under `root`. Returns false, touching
/// nothing, when the store has something in the way it cannot replace: a
/// directory with entries of its own, or a non-directory above `path`.
fn place(root: &Path, path: &str, node: &Node) -> Result<bool> {
  let target = root.join(path);
  let mut above = target.parent();
  while let Some(dir) = above {
    if dir == root || !dir.starts_with(root) {
      break;
    }
    if std::fs::symlink_metadata(dir).is_ok_and(|m| !m.is_dir()) {
      return Ok(false);
    }
    above = dir.parent();
  }
  match std::fs::symlink_metadata(&target) {
    Ok(meta) if meta.is_dir() && node.kind == Kind::Dir => return Ok(true),
    Ok(_) if !remove(&target)? => return Ok(false),
    _ => {}
  }
  copy(node, &target)?;
  Ok(true)
}

/// Copies the run's copy of `path`, if the run has one, under `aside`.
fn keep_aside(run: &BTreeMap<String, Node>, path: &str, aside: &Path) -> Result<()> {
  let Some(node) = run.get(path) else {
    return Ok(());
  };
  let target = aside.join(path);
  if std::fs::symlink_metadata(&target).is_ok_and(|m| !(m.is_dir() && node.kind == Kind::Dir)) {
    std::fs::remove_file(&target).or_else(|_| std::fs::remove_dir_all(&target)).with_context(|| format!("replace {}", target.display()))?;
  }
  copy(node, &target)
}

/// Copies one entry to `target`; a file through a temporary file renamed
/// over it, so a reader never sees half a file.
fn copy(node: &Node, target: &Path) -> Result<()> {
  if let Some(parent) = target.parent() {
    std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
  }
  match &node.kind {
    Kind::Dir => std::fs::create_dir_all(target).with_context(|| format!("create {}", target.display())),
    Kind::File => {
      let partial = partial(target);
      std::fs::copy(&node.path, &partial).with_context(|| format!("copy {} to {}", node.path.display(), partial.display()))?;
      std::fs::rename(&partial, target).with_context(|| format!("rename {} to {}", partial.display(), target.display()))
    }
    Kind::Link(link) => symlink(link, target),
  }
}

#[cfg(unix)]
fn symlink(link: &Path, target: &Path) -> Result<()> {
  std::os::unix::fs::symlink(link, target).with_context(|| format!("link {}", target.display()))
}

// NOTICE: symbolic links are merged on Unix only, where the daemon that
// merges runs; elsewhere a link the run made is an error, not a silent skip.
#[cfg(not(unix))]
fn symlink(_link: &Path, target: &Path) -> Result<()> {
  anyhow::bail!("cannot merge the symbolic link {} on this platform", target.display())
}

/// The temporary file a copy to `target` is written to before its rename.
fn partial(target: &Path) -> PathBuf {
  let mut name = target.file_name().unwrap_or_default().to_os_string();
  name.push(".needy-merge");
  target.with_file_name(name)
}

/// What [`Choice::Lines`] appends to the store's copy: the bytes the run
/// appended to the seed's. `None` when it does not apply: an entry that is
/// not a file on every side that has it, a store that removed it, or a run
/// that changed more than its end. `Some` of nothing when the store already
/// ends with them, so a merge run twice appends once.
// NOTICE: "the store already ends with them" cannot tell a second merge of
// one run from another run that appended the very same bytes last; the
// second then appends nothing. Accepted: it keeps a repeated merge after a
// crash idempotent, and an append-only log rarely gets one identical tail
// from two runs.
fn appended(seed: Option<&Node>, run: Option<&Node>, store: Option<&Node>) -> Result<Option<Vec<u8>>> {
  let file = |node: Option<&Node>| node.is_none_or(|n| n.kind == Kind::File);
  let (Some(run), Some(store)) = (run, store) else {
    return Ok(None);
  };
  if !file(Some(run)) || !file(Some(store)) || !file(seed) {
    return Ok(None);
  }
  let seed = match seed {
    Some(seed) => std::fs::read(&seed.path).with_context(|| format!("read {}", seed.path.display()))?,
    None => Vec::new(),
  };
  let run = std::fs::read(&run.path).with_context(|| format!("read {}", run.path.display()))?;
  let Some(tail) = run.strip_prefix(seed.as_slice()) else {
    return Ok(None);
  };
  let current = std::fs::read(&store.path).with_context(|| format!("read {}", store.path.display()))?;
  if current.ends_with(tail) {
    return Ok(Some(Vec::new()));
  }
  let mut bytes = Vec::new();
  if !current.is_empty() && !current.ends_with(b"\n") {
    bytes.push(b'\n');
  }
  bytes.extend_from_slice(tail);
  Ok(Some(bytes))
}

/// Appends `bytes` to the file at `target`, through a copy renamed over it.
fn append(target: &Path, bytes: &[u8]) -> Result<()> {
  if bytes.is_empty() {
    return Ok(());
  }
  let partial = partial(target);
  std::fs::copy(target, &partial).with_context(|| format!("copy {}", target.display()))?;
  let mut file = std::fs::OpenOptions::new().append(true).open(&partial).with_context(|| format!("open {}", partial.display()))?;
  std::io::Write::write_all(&mut file, bytes).with_context(|| format!("append to {}", partial.display()))?;
  drop(file);
  std::fs::rename(&partial, target).with_context(|| format!("rename {} to {}", partial.display(), target.display()))
}

#[cfg(test)]
#[path = "merge_test.rs"]
mod tests;
