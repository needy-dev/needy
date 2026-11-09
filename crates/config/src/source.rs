//! Where a profile or recipe comes from (a file, `builtin:NAME` or text), and
//! reading it together with its `extends` chain into one merged document.

use std::borrow::Cow;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde_yaml::{Mapping, Value};

use crate::{Error, Kind, builtin, merge};

/// How many parents an `extends` chain may have.
const MAX_DEPTH: usize = 16;

/// One profile or recipe document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
  File(PathBuf),
  Builtin(String),
  /// YAML that was read elsewhere, such as a profile the CLI sent to the
  /// daemon; relative paths in it resolve against `base_dir`.
  Text {
    text: String,
    base_dir: PathBuf,
  },
}

impl Source {
  pub fn file(path: impl Into<PathBuf>) -> Self {
    Self::File(path.into())
  }

  pub fn builtin(name: impl Into<String>) -> Self {
    Self::Builtin(name.into())
  }

  pub fn text(text: impl Into<String>, base_dir: impl Into<PathBuf>) -> Self {
    Self::Text {
      text: text.into(),
      base_dir: base_dir.into(),
    }
  }

  /// A reference as users write it in `extends` or `actor.recipe`:
  /// `builtin:NAME`, or a path relative to `base_dir`.
  pub fn reference(reference: &str, base_dir: &Path) -> Self {
    match reference.strip_prefix("builtin:") {
      Some(name) => Self::builtin(name),
      None => Self::File(base_dir.join(reference)),
    }
  }

  /// The directory that relative paths in this document resolve against. A
  /// built-in has none, so its relative paths stay relative.
  pub fn base_dir(&self) -> PathBuf {
    match self {
      Self::File(path) => path.parent().map(Path::to_path_buf).unwrap_or_default(),
      Self::Builtin(_) => PathBuf::new(),
      Self::Text { base_dir, .. } => base_dir.clone(),
    }
  }
}

impl fmt::Display for Source {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::File(path) => write!(f, "{}", path.display()),
      Self::Builtin(name) => write!(f, "builtin:{name}"),
      Self::Text { base_dir, .. } => write!(f, "(text in {})", base_dir.display()),
    }
  }
}

/// Reads `source` and every parent it extends, and merges them, parents
/// first, with the kind's merge rules.
///
/// TODO(config): relative paths inside a parent (such as a recipe's
/// `network.metadata.file`) resolve against the loaded document's directory,
/// not the parent's. No example needs otherwise; per-file base directories
/// need the merged document to remember each value's origin.
pub(crate) fn read_chain(source: &Source, kind: Kind, rules: &merge::Rules) -> Result<Value, Error> {
  let mut chain = Vec::new();
  read(source, kind, rules, &mut chain).map(Value::Mapping)
}

fn read(source: &Source, kind: Kind, rules: &merge::Rules, chain: &mut Vec<Source>) -> Result<Mapping, Error> {
  let origin = || source.to_string();
  if chain.len() > MAX_DEPTH {
    return Err(Error::TooDeep {
      kind,
      origin: chain[0].to_string(),
      limit: MAX_DEPTH,
    });
  }
  let text: Cow<str> = match source {
    Source::File(path) => Cow::Owned(std::fs::read_to_string(path).map_err(|source| Error::Read {
      kind,
      path: path.clone(),
      source,
    })?),
    Source::Builtin(name) => Cow::Borrowed(builtin::find(kind, name).ok_or_else(|| Error::UnknownBuiltin {
      kind,
      origin: chain.last().map(Source::to_string).unwrap_or_else(origin),
      name: name.clone(),
    })?),
    Source::Text { text, .. } => Cow::Borrowed(text),
  };
  let value: Value = serde_yaml::from_str(&text).map_err(|source| Error::Yaml {
    kind,
    origin: origin(),
    source,
  })?;
  // An empty document, or one of only comments, is an empty mapping.
  let mut document = match value {
    Value::Null => Mapping::new(),
    Value::Mapping(map) => map,
    _ => {
      return Err(Error::NotMapping {
        kind,
        origin: origin(),
      });
    }
  };
  let parent = match document.remove("extends") {
    None | Some(Value::Null) => return Ok(document),
    Some(Value::String(parent)) => Source::reference(&parent, &source.base_dir()),
    Some(_) => {
      return Err(Error::ExtendsNotString {
        kind,
        origin: origin(),
      });
    }
  };
  chain.push(source.clone());
  if chain.contains(&parent) {
    return Err(Error::Cycle {
      kind,
      origin: origin(),
      parent: parent.to_string(),
    });
  }
  let parent = read(&parent, kind, rules, chain)?;
  Ok(merge::merge(parent, document, rules))
}

/// A merged document as YAML text, for a client to send.
pub(crate) fn serialize(value: &Value, kind: Kind, source: &Source) -> Result<String, Error> {
  serde_yaml::to_string(value).map_err(|source_error| Error::Yaml {
    kind,
    origin: source.to_string(),
    source: source_error,
  })
}

/// `path` resolved against `base` when it is relative; `~` stays for the
/// reader's home, and the text after the base keeps its form, such as a
/// trailing `/`.
pub(crate) fn absolute(path: &str, base: &Path) -> String {
  if path.is_empty() || path.starts_with('~') || Path::new(path).is_absolute() {
    return path.to_string();
  }
  let base = std::path::absolute(base).unwrap_or_else(|_| base.to_path_buf());
  format!("{}/{}", base.display().to_string().trim_end_matches('/'), path.trim_start_matches("./"))
}

/// Types a merged document. Errors name the key path, such as
/// `commands[2].when`, in the merged document.
pub(crate) fn deserialize<T: DeserializeOwned>(value: Value, kind: Kind, source: &Source) -> Result<T, Error> {
  serde_path_to_error::deserialize(value).map_err(|err| Error::Invalid {
    kind,
    origin: source.to_string(),
    path: err.path().to_string(),
    message: err.into_inner().to_string(),
  })
}
