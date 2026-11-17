//! One merge model for profiles, recipes and image files, applied to the
//! YAML documents of an `extends` chain before they are typed:
//!
//! - Maps merge key by key, recursively.
//! - A child's scalar or list replaces the parent's. A child's `null` removes
//!   the parent's value, so the field takes its default.
//! - Top-level lists the schema declares as prepend lists (a profile's
//!   `commands` and `resolvers`) keep the parent's entries after the child's,
//!   so the child's rules are evaluated first.
//! - Lists the schema declares as keyed append lists (an image definition's
//!   `layers`) keep the parent's entries first and add the child's after
//!   them; a child entry whose key equals a parent entry's replaces that
//!   entry in place instead.

use serde_yaml::{Mapping, Value};

/// What a document kind declares about merging.
pub(crate) struct Rules {
  /// Top-level keys whose lists are prepended instead of replaced.
  pub prepend: &'static [&'static str],
  /// Lists that append instead of replacing, by their key path from the
  /// document's root.
  pub append: &'static [Append],
}

/// A list that appends along `extends`.
pub(crate) struct Append {
  /// The keys from the document's root to the list, such as
  /// `["actor", "image", "layers"]`.
  pub path: &'static [&'static str],
  /// What makes two entries the same, so a child's replaces its parent's in
  /// place; `None` for an entry that is never the same as another.
  pub key: fn(&Value) -> Option<String>,
}

/// Merges `child` over `parent`.
pub(crate) fn merge(parent: Mapping, child: Mapping, rules: &Rules) -> Mapping {
  merge_map(parent, child, rules, &mut Vec::new())
}

/// Merges `child` over `parent`, the map at `path` in the document.
fn merge_map(mut parent: Mapping, child: Mapping, rules: &Rules, path: &mut Vec<String>) -> Mapping {
  for (key, value) in child {
    if value.is_null() {
      parent.remove(&key);
      continue;
    }
    let name = key.as_str().unwrap_or_default().to_string();
    let prepend = path.is_empty() && rules.prepend.contains(&name.as_str());
    path.push(name);
    let append = rules.append.iter().find(|append| append.path.iter().eq(path.iter()));
    let merged = match (parent.remove(&key), value) {
      (Some(Value::Sequence(old)), Value::Sequence(mut new)) => match append {
        Some(append) => Value::Sequence(append_keyed(old, new, append.key)),
        None if prepend => {
          new.extend(old);
          Value::Sequence(new)
        }
        None => Value::Sequence(new),
      },
      (Some(Value::Mapping(old)), Value::Mapping(new)) => Value::Mapping(merge_map(old, new, rules, path)),
      (_, value) => value,
    };
    path.pop();
    parent.insert(key, merged);
  }
  parent
}

/// The parent's entries, each replaced by the child's entry of the same key,
/// then the child's other entries in their order.
fn append_keyed(mut parent: Vec<Value>, child: Vec<Value>, key: fn(&Value) -> Option<String>) -> Vec<Value> {
  let inherited = parent.len();
  for entry in child {
    let same = key(&entry).and_then(|k| parent[..inherited].iter().position(|old| key(old).as_deref() == Some(k.as_str())));
    match same {
      Some(index) => parent[index] = entry,
      None => parent.push(entry),
    }
  }
  parent
}
