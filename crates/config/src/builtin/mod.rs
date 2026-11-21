//! The embedded catalog, referenced as `builtin:NAME`. Built-in recipes are
//! machines that pass the recipe consistency check; start from one and
//! override a few fields to stay consistent. Built-in profiles carry their
//! image definitions inline so users can start an agent without cloning the
//! repository.

use crate::Kind;

const RECIPES: &[(&str, &str)] = &[
  ("gce-ubuntu-22.04", include_str!("recipes/gce-ubuntu-22.04.yaml")),
  ("dell-optiplex-ubuntu-24.04", include_str!("recipes/dell-optiplex-ubuntu-24.04.yaml")),
];

const PROFILES: &[(&str, &str)] = &[
  ("codex", include_str!("profiles/codex.yaml")),
  ("opencode", include_str!("profiles/opencode.yaml")),
  ("pi", include_str!("profiles/pi.yaml")),
  ("herdr", include_str!("profiles/herdr.yaml")),
];

/// The names of the built-in recipes.
pub fn recipes() -> impl Iterator<Item = &'static str> {
  RECIPES.iter().map(|(name, _)| *name)
}

/// The names of the built-in profiles.
pub fn profiles() -> impl Iterator<Item = &'static str> {
  PROFILES.iter().map(|(name, _)| *name)
}

/// The raw YAML of a built-in profile by name (without the `builtin:` prefix).
pub fn profile_yaml(name: &str) -> Option<&'static str> {
  PROFILES.iter().find(|(n, _)| *n == name).map(|(_, text)| *text)
}

/// The YAML of a built-in document.
pub(crate) fn find(kind: Kind, name: &str) -> Option<&'static str> {
  match kind {
    Kind::Recipe => RECIPES.iter().find(|(n, _)| *n == name).map(|(_, text)| *text),
    Kind::Profile => profile_yaml(name),
    // NOTICE: needy ships no built-in images (owner decision, 2026-11-17,
    // in the images and run data spec): agents are features.
    Kind::Image => None,
  }
}
