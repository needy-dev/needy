//! Collapsing an edit into an image's commit layer: one layer that shows,
//! over any stack below, what the commit layer and then the edit would.

use std::path::Path;

use super::Result;
use super::archive::{Index, Kind};

/// `upper` applied over `lower`, as one layer. `upper`'s entries win;
/// `lower`'s entries that `upper` removes, hides or replaces are dropped;
/// both layers' whiteouts and opaque directories stay for the layers below,
/// except where `upper` puts an entry back over a `lower` whiteout.
pub(super) fn collapse(lower: &Index, upper: &Index) -> Result<Index> {
  let mut merged = Index::default();
  for (at, entry) in &lower.entries {
    if !hidden(upper, at) && !upper.entries.contains_key(at) {
      merged.entries.insert(at.clone(), entry.clone());
    }
  }
  // A hard link in `lower` whose target `upper` removed or replaced keeps
  // the content it had: it becomes a copy of its target as `lower` had it.
  let broken: Vec<_> = merged
    .entries
    .iter()
    .filter_map(|(at, entry)| match &entry.kind {
      Kind::Hardlink(target) if !merged.entries.contains_key(target) => Some(at.clone()),
      _ => None,
    })
    .collect();
  for at in broken {
    let copy = lower.resolve(&at)?;
    merged.entries.insert(at, copy);
  }
  for removed in &lower.whiteouts {
    if hidden(upper, removed) {
      continue;
    }
    match upper.entries.get(removed) {
      // Put back as a directory, it must still hide what `lower` removed
      // below it: it becomes opaque.
      Some(entry) if entry.is_dir() => {
        merged.opaque.insert(removed.clone());
      }
      // Put back as anything else, it replaces what is below anyway.
      Some(_) => {}
      None => {
        merged.whiteouts.insert(removed.clone());
      }
    }
  }
  for dir in &lower.opaque {
    let replaced = upper.entries.get(dir).is_some_and(|entry| !entry.is_dir());
    if !hidden(upper, dir) && !replaced {
      merged.opaque.insert(dir.clone());
    }
  }
  merged.entries.extend(upper.entries.iter().map(|(at, entry)| (at.clone(), entry.clone())));
  merged.whiteouts.extend(upper.whiteouts.iter().cloned());
  merged.opaque.extend(upper.opaque.iter().cloned());
  Ok(merged)
}

/// Whether `upper` hides `at` in the layers below it: it removes `at` or a
/// directory above it, makes a directory above it opaque, or puts something
/// other than a directory above it.
fn hidden(upper: &Index, at: &Path) -> bool {
  at.ancestors().enumerate().any(|(depth, path)| {
    upper.whiteouts.contains(path)
      || (depth > 0 && (upper.opaque.contains(path) || upper.entries.get(path).is_some_and(|entry| !entry.is_dir())))
  })
}
