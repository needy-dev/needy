//! The embedded catalog, referenced as `builtin:NAME`. Built-in recipes are
//! machines that pass the recipe consistency check; start from one and
//! override a few fields to stay consistent.

use crate::Kind;

const RECIPES: &[(&str, &str)] = &[
  ("gce-ubuntu-22.04", include_str!("recipes/gce-ubuntu-22.04.yaml")),
  ("dell-optiplex-ubuntu-24.04", include_str!("recipes/dell-optiplex-ubuntu-24.04.yaml")),
];

/// The names of the built-in recipes.
pub fn recipes() -> impl Iterator<Item = &'static str> {
  RECIPES.iter().map(|(name, _)| *name)
}

/// The YAML of a built-in document.
pub(crate) fn find(kind: Kind, name: &str) -> Option<&'static str> {
  match kind {
    Kind::Recipe => RECIPES.iter().find(|(n, _)| *n == name).map(|(_, text)| *text),
    // NOTICE: there are no built-in profiles; `builtin:` in a profile's
    // `extends` names none until one is added here.
    Kind::Profile => None,
  }
}
