//! What users write: profile and recipe schemas, loading with `extends`,
//! one merge model, validation of cross-references, and the built-in catalog.
//!
//! - [`source`]: where a document comes from (a file, `builtin:NAME` or text)
//!   and its `extends` chain.
//! - [`profile`] and [`recipe`]: the schemas and `load`.
//! - [`validate`]: cross-references inside a loaded profile, as findings.
//! - [`value`]: values users write, such as durations and sizes.
//!
//! Interpreting a profile (rules, recipe compilation, chaos) is `needy-core`'s
//! job; this crate never sees processes, sockets or gRPC.

pub mod builtin;
mod merge;
pub mod profile;
pub mod recipe;
pub mod source;
pub mod validate;
pub mod value;

use std::fmt;
use std::path::PathBuf;

pub use source::Source;

/// The kind of document being loaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
  Profile,
  Recipe,
}

impl fmt::Display for Kind {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(match self {
      Self::Profile => "profile",
      Self::Recipe => "recipe",
    })
  }
}

/// Why a profile or recipe could not be loaded.
#[derive(Debug, thiserror::Error)]
pub enum Error {
  #[error("cannot read {kind} {}", path.display())]
  Read {
    kind: Kind,
    path: PathBuf,
    #[source]
    source: std::io::Error,
  },
  #[error("invalid YAML in {kind} {origin}")]
  Yaml {
    kind: Kind,
    origin: String,
    #[source]
    source: serde_yaml::Error,
  },
  #[error("{kind} {origin} is not a mapping")]
  NotMapping { kind: Kind, origin: String },
  #[error("{kind} {origin}: `extends` must be a string")]
  ExtendsNotString { kind: Kind, origin: String },
  #[error("{kind} {origin}: no built-in {kind} named {name:?}")]
  UnknownBuiltin {
    kind: Kind,
    origin: String,
    name: String,
  },
  #[error("{kind} {origin}: `extends` nests more than {limit} levels")]
  TooDeep {
    kind: Kind,
    origin: String,
    limit: usize,
  },
  #[error("{kind} {origin}: `extends` cycles back to {parent}")]
  Cycle {
    kind: Kind,
    origin: String,
    parent: String,
  },
  /// The merged document does not fit the schema: an unknown key, a missing
  /// field or a value of the wrong form. `path` is the key path in the merged
  /// document, such as `commands[2].when`.
  #[error("invalid {kind} {origin}: {}{message}", at(path))]
  Invalid {
    kind: Kind,
    origin: String,
    path: String,
    message: String,
  },
}

/// `PATH: ` before a message, or nothing for the document root.
fn at(path: &str) -> String {
  if path.is_empty() || path == "." {
    String::new()
  } else {
    format!("{path}: ")
  }
}
