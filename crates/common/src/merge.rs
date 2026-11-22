//! Three-way merge of a directory tree: a run's copy of a tree merged back
//! into the store it was seeded from, against that seed, entry by entry.
//!
//! The merge touches only the directories it is given. It knows no file
//! format: entries are compared by content, and the one text-aware choice,
//! [`Choice::Lines`], treats a file as append-only bytes.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek, SeekFrom};
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
/// seed, or hands matching entries to a merge driver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
  /// The store's copy stays; the run's is dropped without a conflict.
  Store,
  /// The run's copy replaces the store's.
  Run,
  /// An append-only text file: the bytes the run appended to the seed's
  /// copy are appended to the store's. A run that did more than append is a
  /// conflict.
  Lines,
  /// A merge driver command: needy's built-in file merge skips every entry
  /// matching this pattern across all three trees and returns them in
  /// [`Outcome::driver_claims`] for the daemon to hand to the driver.
  Driver(String),
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

/// Entries claimed by one merge driver command, skipped by needy's built-in
/// file merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriverClaim {
  /// The driver command from `Choice::Driver`.
  pub driver: String,
  /// Every relative path (across seed, run, and store) matching a rule for
  /// this driver, in path order. Includes side files such as `-wal` / `-shm`.
  pub paths: Vec<String>,
}

/// Every entry the merge acted on, in path order, plus any entries claimed by
/// merge drivers. Entries the run left as the seed had them, or changed
/// exactly as the store did, are not listed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outcome {
  pub changes: Vec<Change>,
  pub driver_claims: Vec<DriverClaim>,
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
  // Driver claims preserve the order in which each driver command first
  // appears in `policy.on_conflict`.
  let mut driver_order: Vec<String> = Vec::new();
  let mut driver_paths: BTreeMap<String, Vec<String>> = BTreeMap::new();
  let mut driver_dirty: BTreeMap<String, bool> = BTreeMap::new();

  let mut scratch = CompareScratch::default();

  for path in paths {
    let (s, r, t) = (seed.get(path), run.get(path), store.get(path));
    if let Some(Choice::Driver(driver)) = choice(policy, path) {
      if !driver_paths.contains_key(driver) {
        driver_order.push(driver.to_string());
      }
      driver_paths.entry(driver.to_string()).or_default().push(path.clone());
      if !(same(s, r, &mut scratch)? || same(t, r, &mut scratch)?) {
        driver_dirty.insert(driver.to_string(), true);
      }
      continue;
    }
    if same(s, r, &mut scratch)? || same(t, r, &mut scratch)? {
      continue;
    }
    let step = if same(t, s, &mut scratch)? {
      Step::Take
    } else {
      match choice(policy, path) {
        Some(Choice::Store) => Step::Keep,
        Some(Choice::Run) => Step::TakeChosen,
        Some(Choice::Lines) => Step::Lines,
        Some(Choice::Driver(_)) => unreachable!("driver claims handled above"),
        None => Step::Aside,
      }
    };
    plan.push((path.clone(), step));
  }

  let driver_claims: Vec<DriverClaim> = driver_order
    .into_iter()
    .filter(|driver| driver_dirty.get(driver).copied().unwrap_or(false))
    .filter_map(|driver| driver_paths.remove(&driver).map(|paths| DriverClaim { driver, paths }))
    .collect();

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
      Step::Lines => match appended(seed.get(path), run.get(path), store.get(path), &mut scratch)? {
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
    driver_claims,
  })
}

/// Stages the three trees (`base/`, `run/`, `current/`) and an empty `merged/`
/// directory under `staging` containing only the entries in `paths`.
pub fn stage_driver_trees(trees: Trees<'_>, paths: &[String], staging: &Path) -> Result<()> {
  for (name, src) in [
    ("base", trees.seed),
    ("run", trees.run),
    ("current", trees.store),
  ] {
    let dst = staging.join(name);
    std::fs::create_dir_all(&dst).with_context(|| format!("create {}", dst.display()))?;
    for rel in paths {
      copy_relative(src, &dst, rel)?;
    }
  }
  let merged = staging.join("merged");
  std::fs::create_dir_all(&merged).with_context(|| format!("create {}", merged.display()))?;
  Ok(())
}

/// Atomically applies the merged files written by a merge driver in `out` back
/// into `store`, and removes any `claimed` entries absent from `out` (such as
/// checkpointed `-wal` / `-shm` side files). Returns the relative paths placed.
pub fn apply_driver_output(out: &Path, store: &Path, claimed: &[String]) -> Result<Vec<String>> {
  std::fs::create_dir_all(store).with_context(|| format!("create store {}", store.display()))?;
  let produced = scan(out, &[])?;
  let mut placed = Vec::new();
  for (rel, node) in &produced {
    if !place(store, rel, node)? {
      anyhow::bail!("cannot place merge driver output {rel} into store {}", store.display());
    }
    placed.push(rel.clone());
  }
  // Remove stale claimed entries not present in `out` (deepest first so
  // emptied directories can be removed cleanly).
  let mut stale: Vec<&String> = claimed.iter().filter(|rel| !produced.contains_key(*rel)).collect();
  stale.sort();
  for rel in stale.into_iter().rev() {
    let _ = remove(&store.join(rel))?;
  }
  Ok(placed)
}

/// Copies the run's copies of `paths` into `aside` when a merge driver fails
/// or times out, leaving the store untouched.
pub fn keep_driver_conflicts(run: &Path, paths: &[String], aside: &Path) -> Result<()> {
  for rel in paths {
    copy_relative(run, aside, rel)?;
  }
  Ok(())
}

/// Copies a single relative path from `src` to `dst` when it exists in `src`.
fn copy_relative(src: &Path, dst: &Path, rel: &str) -> Result<()> {
  let from = src.join(rel);
  let meta = match std::fs::symlink_metadata(&from) {
    Ok(meta) => meta,
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
    Err(e) => return Err(e).with_context(|| format!("stat {}", from.display())),
  };
  let kind = if meta.is_dir() {
    Kind::Dir
  } else if meta.is_symlink() {
    Kind::Link(std::fs::read_link(&from).with_context(|| format!("read link {}", from.display()))?)
  } else {
    Kind::File
  };
  let target = dst.join(rel);
  if std::fs::symlink_metadata(&target).is_ok_and(|m| !(m.is_dir() && kind == Kind::Dir)) {
    std::fs::remove_file(&target).or_else(|_| std::fs::remove_dir_all(&target)).with_context(|| format!("replace {}", target.display()))?;
  }
  copy(&Node { path: from, kind }, &target)
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

fn choice<'a>(policy: &'a Policy, path: &str) -> Option<&'a Choice> {
  policy.on_conflict.iter().find(|(pattern, _)| matches(pattern, path)).map(|(_, choice)| choice)
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

const COMPARE_CHUNK: usize = 64 * 1024;

/// Reusable chunk buffers for file comparisons during one merge pass.
#[derive(Default)]
struct CompareScratch {
  a: Vec<u8>,
  b: Vec<u8>,
}

impl CompareScratch {
  fn buffers(&mut self) -> (&mut [u8], &mut [u8]) {
    if self.a.is_empty() {
      self.a.resize(COMPARE_CHUNK, 0);
      self.b.resize(COMPARE_CHUNK, 0);
    }
    (&mut self.a, &mut self.b)
  }
}

/// Whether two entries are the same: both absent, or of one kind with the
/// same content.
fn same(a: Option<&Node>, b: Option<&Node>, scratch: &mut CompareScratch) -> Result<bool> {
  match (a, b) {
    (None, None) => Ok(true),
    (Some(a), Some(b)) => match (&a.kind, &b.kind) {
      (Kind::File, Kind::File) => same_content(&a.path, &b.path, scratch),
      (a, b) => Ok(a == b),
    },
    _ => Ok(false),
  }
}

fn same_content(a: &Path, b: &Path, scratch: &mut CompareScratch) -> Result<bool> {
  let (ma, mb) = (std::fs::metadata(a)?, std::fs::metadata(b)?);
  if ma.len() != mb.len() {
    return Ok(false);
  }
  #[cfg(unix)]
  {
    use std::os::unix::fs::MetadataExt;
    if ma.dev() == mb.dev() && ma.ino() == mb.ino() {
      return Ok(true);
    }
  }
  if ma.len() == 0 {
    return Ok(true);
  }
  let open = |p: &Path| std::fs::File::open(p).with_context(|| format!("open {}", p.display()));
  let (mut fa, mut fb) = (open(a)?, open(b)?);
  let (ba, bb) = scratch.buffers();
  loop {
    let na = read_full(&mut fa, ba)?;
    let nb = read_full(&mut fb, bb)?;
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
fn appended(seed: Option<&Node>, run: Option<&Node>, store: Option<&Node>, scratch: &mut CompareScratch) -> Result<Option<Vec<u8>>> {
  let file = |node: Option<&Node>| node.is_none_or(|n| n.kind == Kind::File);
  let (Some(run), Some(store)) = (run, store) else {
    return Ok(None);
  };
  if !file(Some(run)) || !file(Some(store)) || !file(seed) {
    return Ok(None);
  }
  let mut run_file = std::fs::File::open(&run.path).with_context(|| format!("open {}", run.path.display()))?;
  let run_len = run_file.metadata().with_context(|| format!("stat {}", run.path.display()))?.len();
  if let Some(seed) = seed {
    let mut seed_file = std::fs::File::open(&seed.path).with_context(|| format!("open {}", seed.path.display()))?;
    let seed_len = seed_file.metadata().with_context(|| format!("stat {}", seed.path.display()))?.len();
    if run_len < seed_len {
      return Ok(None);
    }
    if seed_len > 0 {
      let (ba, bb) = scratch.buffers();
      let mut remaining = seed_len;
      while remaining > 0 {
        let want = usize::try_from(remaining.min(ba.len() as u64)).expect("bounded by buffer len");
        let na = read_full(&mut seed_file, &mut ba[..want])?;
        let nb = read_full(&mut run_file, &mut bb[..want])?;
        if na != want || nb != want || ba[..want] != bb[..want] {
          return Ok(None);
        }
        remaining -= want as u64;
      }
    }
  }
  let mut tail = Vec::new();
  run_file.read_to_end(&mut tail).with_context(|| format!("read {}", run.path.display()))?;
  if tail.is_empty() {
    return Ok(Some(Vec::new()));
  }
  let mut store_file = std::fs::File::open(&store.path).with_context(|| format!("open {}", store.path.display()))?;
  let store_len = store_file.metadata().with_context(|| format!("stat {}", store.path.display()))?.len();
  let tail_len = tail.len() as u64;
  if store_len >= tail_len {
    store_file.seek(SeekFrom::Start(store_len - tail_len)).with_context(|| format!("seek {}", store.path.display()))?;
    let mut suffix = vec![0u8; tail.len()];
    if read_full(&mut store_file, &mut suffix)? == tail.len() && suffix == tail {
      return Ok(Some(Vec::new()));
    }
  }
  let needs_newline = if store_len > 0 {
    store_file.seek(SeekFrom::Start(store_len - 1)).with_context(|| format!("seek {}", store.path.display()))?;
    let mut last = [0u8; 1];
    read_full(&mut store_file, &mut last)? == 1 && last[0] != b'\n'
  } else {
    false
  };
  let mut bytes = Vec::with_capacity(tail.len() + usize::from(needs_newline));
  if needs_newline {
    bytes.push(b'\n');
  }
  bytes.extend_from_slice(&tail);
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
