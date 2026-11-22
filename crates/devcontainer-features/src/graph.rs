//! Dev Container feature dependency graph resolution: transitive `dependsOn`
//! expansion, cycle detection, and deterministic `installsAfter` topological
//! sorting (Kahn's algorithm preserving user declaration order on ties).
//!
//! Reference: https://containers.dev/implementors/features/#installation-order

use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap, HashMap, HashSet};

use crate::metadata::{FeatureMetadata, OptionValue, untagged};

/// One requested Dev Container feature and its user-supplied option values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeatureRequest {
  /// Feature reference as declared (e.g. `ghcr.io/devcontainers/features/node:1`).
  pub reference: String,
  /// Option values supplied for this feature.
  pub options: BTreeMap<String, OptionValue>,
}

/// One feature in resolved installation order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderedFeature<'a> {
  /// Feature reference as declared or pulled in via `dependsOn`.
  pub reference: String,
  /// Parsed `devcontainer-feature.json` metadata.
  pub metadata: &'a FeatureMetadata,
  /// Option values for this feature (from user declaration or `dependsOn`).
  pub options: BTreeMap<String, OptionValue>,
}

/// Errors resolving a Dev Container feature installation order.
#[derive(Debug, thiserror::Error)]
pub enum GraphError {
  #[error("feature `{reference}` was not resolved before planning")]
  MissingFeature { reference: String },
  #[error("feature `dependsOn` cycle detected: {cycle}")]
  DependencyCycle { cycle: String },
  #[error("feature ordering cycle detected across `dependsOn`/`installsAfter`: {cycle}")]
  OrderingCycle { cycle: String },
}

/// Expands transitive `dependsOn` dependencies, rejects cycles, and returns
/// all features in deterministic topological order honoring both hard
/// `dependsOn` edges and soft `installsAfter` edges (ignoring absent
/// `installsAfter` targets per the Dev Container specification).
///
/// Ties among features with no ordering constraints between them are broken
/// deterministically by preserving user declaration order.
pub fn resolve_order<'a>(
  requested: &[FeatureRequest],
  features: &'a BTreeMap<String, FeatureMetadata>,
) -> Result<Vec<OrderedFeature<'a>>, GraphError> {
  resolve_order_excluding(requested, features, &HashSet::new())
}

/// Like [`resolve_order`], but skips any feature whose untagged canonical
/// identity is already in `already_installed`.
pub fn resolve_order_excluding<'a>(
  requested: &[FeatureRequest],
  features: &'a BTreeMap<String, FeatureMetadata>,
  already_installed: &HashSet<String>,
) -> Result<Vec<OrderedFeature<'a>>, GraphError> {
  // Phase 1: expand transitive `dependsOn` and detect hard dependency cycles.
  let mut nodes: Vec<OrderedFeature<'a>> = Vec::new();
  let mut key_to_idx: HashMap<String, usize> = HashMap::new();
  let mut visiting: Vec<String> = Vec::new();

  for req in requested {
    expand_feature(&req.reference, &req.options, features, already_installed, &mut visiting, &mut key_to_idx, &mut nodes)?;
  }

  let n = nodes.len();
  if n <= 1 {
    return Ok(nodes);
  }

  // Build lookup tables for `dependsOn` (by untagged ref) and `installsAfter`
  // (by untagged ref or bare `metadata.id`).
  let mut id_to_idx: HashMap<&str, usize> = HashMap::new();
  for (idx, node) in nodes.iter().enumerate() {
    id_to_idx.entry(node.metadata.id.as_str()).or_insert(idx);
  }

  // Phase 2: build directed graph `u -> v` ("u must install before v") from
  // both `dependsOn` and present `installsAfter` entries.
  let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
  let mut indegree: Vec<usize> = vec![0; n];
  let mut edges: HashSet<(usize, usize)> = HashSet::new();

  let mut add_edge = |u: usize, v: usize| {
    if u != v && edges.insert((u, v)) {
      adj[u].push(v);
      indegree[v] += 1;
    }
  };

  for (v, node) in nodes.iter().enumerate() {
    for dep_ref in node.metadata.depends_on.keys() {
      let dep_key = untagged(dep_ref);
      if let Some(&u) = key_to_idx.get(dep_key) {
        add_edge(u, v);
      }
    }
    for after in &node.metadata.installs_after {
      let after_key = untagged(after);
      let target = key_to_idx.get(after_key).copied().or_else(|| id_to_idx.get(after.as_str()).copied());
      if let Some(u) = target {
        add_edge(u, v);
      }
    }
  }

  // Phase 3: Kahn's algorithm with a min-heap keyed by original expansion index
  // so ties preserve user declaration order deterministically.
  let mut ready: BinaryHeap<Reverse<usize>> = BinaryHeap::new();
  for (idx, &deg) in indegree.iter().enumerate() {
    if deg == 0 {
      ready.push(Reverse(idx));
    }
  }

  let mut sorted_indices = Vec::with_capacity(n);
  while let Some(Reverse(u)) = ready.pop() {
    sorted_indices.push(u);
    for &v in &adj[u] {
      indegree[v] -= 1;
      if indegree[v] == 0 {
        ready.push(Reverse(v));
      }
    }
  }

  if sorted_indices.len() != n {
    let remaining: Vec<&str> =
      indegree.iter().enumerate().filter(|(_, deg)| **deg > 0).map(|(idx, _)| nodes[idx].reference.as_str()).collect();
    return Err(GraphError::OrderingCycle {
      cycle: remaining.join(" <-> "),
    });
  }

  let mut slots: Vec<Option<OrderedFeature<'a>>> = nodes.into_iter().map(Some).collect();
  let ordered = sorted_indices.into_iter().map(|idx| slots[idx].take().expect("each index visited once")).collect();
  Ok(ordered)
}

fn expand_feature<'a>(
  reference: &str,
  options: &BTreeMap<String, OptionValue>,
  features: &'a BTreeMap<String, FeatureMetadata>,
  already_installed: &HashSet<String>,
  visiting: &mut Vec<String>,
  key_to_idx: &mut HashMap<String, usize>,
  nodes: &mut Vec<OrderedFeature<'a>>,
) -> Result<(), GraphError> {
  let id_key = untagged(reference).to_string();
  if already_installed.contains(&id_key) || key_to_idx.contains_key(&id_key) {
    return Ok(());
  }
  if let Some(pos) = visiting.iter().position(|entry| entry == &id_key) {
    let mut cycle: Vec<&str> = visiting[pos..].iter().map(String::as_str).collect();
    cycle.push(&id_key);
    return Err(GraphError::DependencyCycle {
      cycle: cycle.join(" -> "),
    });
  }

  let metadata = features.get(reference).ok_or_else(|| GraphError::MissingFeature {
    reference: reference.to_string(),
  })?;

  visiting.push(id_key.clone());
  for (dep_ref, dep_options) in &metadata.depends_on {
    expand_feature(dep_ref, dep_options, features, already_installed, visiting, key_to_idx, nodes)?;
  }
  visiting.pop();

  if let std::collections::hash_map::Entry::Vacant(entry) = key_to_idx.entry(id_key) {
    let idx = nodes.len();
    entry.insert(idx);
    nodes.push(OrderedFeature {
      reference: reference.to_string(),
      metadata,
      options: options.clone(),
    });
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  fn meta(json: &str) -> FeatureMetadata {
    FeatureMetadata::parse(json).unwrap()
  }

  fn req(reference: &str) -> FeatureRequest {
    FeatureRequest {
      reference: reference.into(),
      options: BTreeMap::new(),
    }
  }

  #[test]
  fn expands_depends_on_transitively_without_duplicates() {
    let features = BTreeMap::from([
      ("ghcr.io/example/app:1".into(), meta(r#"{"id":"app","dependsOn":{"ghcr.io/example/mid:1":{"mode":"fast"}}}"#)),
      ("ghcr.io/example/mid:1".into(), meta(r#"{"id":"mid","dependsOn":{"ghcr.io/example/base:1":{}}}"#)),
      ("ghcr.io/example/base:1".into(), meta(r#"{"id":"base"}"#)),
    ]);

    let ordered = resolve_order(&[req("ghcr.io/example/app:1")], &features).unwrap();
    let refs: Vec<&str> = ordered.iter().map(|f| f.reference.as_str()).collect();
    assert_eq!(
      refs,
      [
        "ghcr.io/example/base:1",
        "ghcr.io/example/mid:1",
        "ghcr.io/example/app:1"
      ]
    );
    assert_eq!(ordered[1].options.get("mode"), Some(&OptionValue::from("fast")));

    // Explicit base + app deduplicates base.
    let ordered2 = resolve_order(&[req("ghcr.io/example/base:1"), req("ghcr.io/example/app:1")], &features).unwrap();
    assert_eq!(ordered2.len(), 3);
  }

  #[test]
  fn rejects_depends_on_cycle() {
    let features = BTreeMap::from([
      ("ghcr.io/example/a:1".into(), meta(r#"{"id":"a","dependsOn":{"ghcr.io/example/b:1":{}}}"#)),
      ("ghcr.io/example/b:1".into(), meta(r#"{"id":"b","dependsOn":{"ghcr.io/example/a:1":{}}}"#)),
    ]);

    let err = resolve_order(&[req("ghcr.io/example/a:1")], &features).unwrap_err();
    assert!(matches!(err, GraphError::DependencyCycle { .. }));
    let msg = err.to_string();
    assert!(msg.contains("ghcr.io/example/a"), "{msg}");
    assert!(msg.contains("ghcr.io/example/b"), "{msg}");
  }

  #[test]
  fn installs_after_reorders_present_features_and_ignores_absent() {
    let features = BTreeMap::from([
      ("ghcr.io/devcontainers/features/common-utils:2".into(), meta(r#"{"id":"common-utils"}"#)),
      (
        "ghcr.io/devcontainers/features/node:1".into(),
        meta(r#"{"id":"node","installsAfter":["ghcr.io/devcontainers/features/common-utils","ghcr.io/devcontainers/features/missing"]}"#),
      ),
      ("ghcr.io/devcontainers/features/git:1".into(), meta(r#"{"id":"git"}"#)),
    ]);

    // User declares [node, git, common-utils]: node must move after common-utils,
    // while git (unconstrained) preserves its relative position before common-utils.
    let ordered = resolve_order(
      &[
        req("ghcr.io/devcontainers/features/node:1"),
        req("ghcr.io/devcontainers/features/git:1"),
        req("ghcr.io/devcontainers/features/common-utils:2"),
      ],
      &features,
    )
    .unwrap();
    let refs: Vec<&str> = ordered.iter().map(|f| f.reference.as_str()).collect();
    assert_eq!(
      refs,
      [
        "ghcr.io/devcontainers/features/git:1",
        "ghcr.io/devcontainers/features/common-utils:2",
        "ghcr.io/devcontainers/features/node:1",
      ]
    );

    // Absent installsAfter target is ignored when node is requested alone.
    let alone = resolve_order(&[req("ghcr.io/devcontainers/features/node:1")], &features).unwrap();
    assert_eq!(alone.len(), 1);
    assert_eq!(alone[0].reference, "ghcr.io/devcontainers/features/node:1");
  }

  #[test]
  fn installs_after_cycle_returns_ordering_cycle_error() {
    let features = BTreeMap::from([
      ("ghcr.io/example/a:1".into(), meta(r#"{"id":"a","installsAfter":["ghcr.io/example/b"]}"#)),
      ("ghcr.io/example/b:1".into(), meta(r#"{"id":"b","installsAfter":["ghcr.io/example/a"]}"#)),
    ]);

    let err = resolve_order(&[req("ghcr.io/example/a:1"), req("ghcr.io/example/b:1")], &features).unwrap_err();
    assert!(matches!(err, GraphError::OrderingCycle { .. }));
    let msg = err.to_string();
    assert!(msg.contains("ghcr.io/example/a:1"), "{msg}");
    assert!(msg.contains("ghcr.io/example/b:1"), "{msg}");
  }

  #[test]
  fn deterministic_tie_breaking_preserves_declaration_order() {
    let features = BTreeMap::from([
      ("ghcr.io/example/c:1".into(), meta(r#"{"id":"c"}"#)),
      ("ghcr.io/example/a:1".into(), meta(r#"{"id":"a"}"#)),
      ("ghcr.io/example/b:1".into(), meta(r#"{"id":"b"}"#)),
    ]);

    let ordered = resolve_order(
      &[
        req("ghcr.io/example/c:1"),
        req("ghcr.io/example/a:1"),
        req("ghcr.io/example/b:1"),
      ],
      &features,
    )
    .unwrap();
    let refs: Vec<&str> = ordered.iter().map(|f| f.reference.as_str()).collect();
    assert_eq!(
      refs,
      [
        "ghcr.io/example/c:1",
        "ghcr.io/example/a:1",
        "ghcr.io/example/b:1"
      ]
    );
  }
}
