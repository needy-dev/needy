//! One merge model for profiles and recipes, applied to the YAML documents of
//! an `extends` chain before they are typed:
//!
//! - Maps merge key by key, recursively.
//! - A child's scalar or list replaces the parent's. A child's `null` removes
//!   the parent's value, so the field takes its default.
//! - Top-level lists the schema declares as prepend lists (a profile's
//!   `commands` and `resolvers`) keep the parent's entries after the child's,
//!   so the child's rules are evaluated first.

use serde_yaml::{Mapping, Value};

/// What a document kind declares about merging.
pub(crate) struct Rules {
  /// Top-level keys whose lists are prepended instead of replaced.
  pub prepend: &'static [&'static str],
}

/// Merges `child` over `parent`.
pub(crate) fn merge(mut parent: Mapping, child: Mapping, rules: &Rules) -> Mapping {
  for (key, value) in child {
    if value.is_null() {
      parent.remove(&key);
      continue;
    }
    let prepend = key.as_str().is_some_and(|k| rules.prepend.contains(&k));
    let merged = match parent.remove(&key) {
      Some(Value::Sequence(old)) if prepend => match value {
        Value::Sequence(mut new) => {
          new.extend(old);
          Value::Sequence(new)
        }
        other => other,
      },
      Some(old) => merge_value(old, value),
      None => value,
    };
    parent.insert(key, merged);
  }
  parent
}

fn merge_value(parent: Value, child: Value) -> Value {
  match (parent, child) {
    (Value::Mapping(mut parent), Value::Mapping(child)) => {
      for (key, value) in child {
        if value.is_null() {
          parent.remove(&key);
          continue;
        }
        let merged = match parent.remove(&key) {
          Some(old) => merge_value(old, value),
          None => value,
        };
        parent.insert(key, merged);
      }
      Value::Mapping(parent)
    }
    (_, child) => child,
  }
}
