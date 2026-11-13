//! Private, versioned client persistence. One lock covers read/validate/replace,
//! including the remote issuance step of pairing, so conflicts precede code use.
use anyhow::{Context as _, Result};
use needy_config::global::Config;
use std::io::Write as _;
use std::path::{Path, PathBuf};

pub fn load(path: &Path) -> Result<Config> {
  load_inner(path).map_err(|cause| {
    if cause.chain().any(|error| error.is::<crate::diagnostics::Error>()) {
      cause
    } else {
      failure("config.read_failed", "Cannot read the local configuration", path, cause)
    }
  })
}

fn load_inner(path: &Path) -> Result<Config> {
  reject_symlink(path)?;
  if path.try_exists().context("cannot inspect configuration file")? {
    return read(path);
  }
  Ok(Config::default())
}

fn read(path: &Path) -> Result<Config> {
  reject_symlink(path)?;
  let mut options = std::fs::OpenOptions::new();
  options.read(true);
  #[cfg(unix)]
  {
    use std::os::unix::fs::OpenOptionsExt as _;
    options.custom_flags(libc::O_NOFOLLOW);
  }
  let mut file = match options.open(path) {
    Ok(file) => file,
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
    Err(e) => return Err(e).with_context(|| format!("cannot read configuration {}", path.display())),
  };
  let metadata = file.metadata()?;
  anyhow::ensure!(metadata.is_file(), "configuration must be a regular file");
  let mut text = String::new();
  std::io::Read::read_to_string(&mut file, &mut text).context("cannot read configuration")?;
  let raw = serde_yaml::from_str::<serde_yaml::Value>(&text).map_err(|error| {
    let mut diagnostic = crate::diagnostics::Error::new("config.invalid_yaml", "The global document contains invalid YAML")
      .fact("source", path.display().to_string())
      .fact("cause", error.to_string())
      .check("Inspect the YAML location; preserve existing credentials while correcting it.");
    if let Some(location) = error.location() {
      diagnostic = diagnostic.fact("line", location.line().to_string()).fact("column", location.column().to_string());
    }
    diagnostic.cause(error)
  })?;
  let secrets: Vec<_> = raw["contexts"]
    .as_mapping()
    .into_iter()
    .flat_map(|records| records.values())
    .filter_map(|record| record["credential"].as_str())
    .collect();
  let config: Config = serde_path_to_error::deserialize(serde_yaml::Deserializer::from_str(&text)).map_err(|error| {
    let field = error.path().to_string();
    let error = error.into_inner();
    let mut diagnostic = crate::diagnostics::Error::new("config.invalid_field", "The global document does not fit the configuration schema")
      .fact("source", path.display().to_string())
      .fact("field", field)
      .fact("cause", safe_text(&error.to_string(), &secrets))
      .check("Inspect the named field; preserve existing credentials while correcting it.");
    if let Some(location) = error.location() {
      diagnostic = diagnostic.fact("line", location.line().to_string()).fact("column", location.column().to_string());
    }
    diagnostic.cause(error)
  })?;
  config.validate().map_err(|error| invalid(error, path))?;
  // Nonsecret daemon snapshots can be shared for inspection. A document with
  // client credentials must belong to this account and deny group/world access.
  #[cfg(unix)]
  if !config.contexts.is_empty() {
    use std::os::unix::fs::MetadataExt as _;
    anyhow::ensure!(metadata.mode() & 0o077 == 0, "configuration with credentials must be private (chmod 600)");
    // SAFETY: geteuid has no preconditions and changes no process state.
    anyhow::ensure!(metadata.uid() == unsafe { libc::geteuid() }, "configuration with credentials belongs to another OS account");
  }
  Ok(config)
}

/// The file handle keeps the advisory lock for the complete mutation.
pub struct Locked {
  _lock: std::fs::File,
  path: PathBuf,
  pub config: Config,
}

impl Locked {
  pub fn open(path: &Path) -> Result<Self> {
    Self::open_inner(path).map_err(|cause| {
      if cause.chain().any(|error| error.is::<crate::diagnostics::Error>()) {
        cause
      } else {
        failure("config.write_failed", "Cannot lock the local configuration for writing", path, cause)
      }
    })
  }

  fn open_inner(path: &Path) -> Result<Self> {
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    #[cfg(unix)]
    needy_api_server::create_private_dir(dir).with_context(|| format!("cannot create configuration directory {}", dir.display()))?;
    // TODO(windows-acl): persistence remains unsupported until owner-approved ACL
    // support can enforce private storage; Unix modes do not provide Windows ACLs.
    #[cfg(not(unix))]
    anyhow::bail!("private configuration persistence on Windows requires ACL support; see CLI configuration specification");
    let lock_path = path.with_extension("yaml.lock");
    reject_symlink(&lock_path)?;
    let mut options = std::fs::OpenOptions::new();
    options.create(true).read(true).write(true).truncate(false);
    #[cfg(unix)]
    {
      use std::os::unix::fs::OpenOptionsExt as _;
      options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let lock = options.open(&lock_path).with_context(|| format!("cannot open configuration lock {}", lock_path.display()))?;
    lock.lock().with_context(|| format!("cannot lock configuration {}", path.display()))?;
    reject_symlink(path)?;
    let config = read(path)?;
    Ok(Self {
      _lock: lock,
      path: path.to_path_buf(),
      config,
    })
  }

  /// Validate everything before touching the old document. The temporary is
  /// private from its first open; rename publishes it in the same directory.
  pub fn save(&self) -> Result<()> {
    self.config.validate().map_err(|error| invalid(error, &self.path))?;
    self.save_inner().map_err(|cause| failure("config.write_failed", "Cannot write the local configuration", &self.path, cause))
  }

  fn save_inner(&self) -> Result<()> {
    let text = serde_yaml::to_string(&self.config)?;
    let dir = self.path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    let temporary = dir.join(format!(".config-{}.tmp", uuid::Uuid::new_v4()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
      use std::os::unix::fs::OpenOptionsExt as _;
      options.mode(0o600);
    }
    let result = (|| -> Result<()> {
      let mut file = options.open(&temporary).context("cannot create private configuration temporary")?;
      file.write_all(text.as_bytes()).and_then(|()| file.sync_all()).context("cannot write configuration temporary")?;
      std::fs::rename(&temporary, &self.path).with_context(|| format!("cannot replace configuration {}", self.path.display()))?;
      // Rename is the commit point. A later directory-sync failure cannot be
      // reported as an unsaved credential: the new private record is published.
      // Report uncertain crash durability while leaving that record usable.
      if let Err(error) = std::fs::File::open(dir).and_then(|directory| directory.sync_all()) {
        crate::output::message(format_args!("warning: configuration was saved, but directory durability could not be confirmed: {error}"));
      }
      Ok(())
    })();
    if result.is_err()
      && let Err(e) = std::fs::remove_file(&temporary)
      && e.kind() != std::io::ErrorKind::NotFound
    {
      tracing::warn!(error = %e, "cannot clean configuration temporary");
    }
    result
  }
}

fn reject_symlink(path: &Path) -> Result<()> {
  match std::fs::symlink_metadata(path) {
    Ok(metadata) if metadata.file_type().is_symlink() => {
      anyhow::bail!("refusing symbolic-link configuration destination {}", path.display())
    }
    Ok(metadata) => {
      anyhow::ensure!(metadata.is_file(), "configuration destination must be a regular file: {}", path.display());
      Ok(())
    }
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
    Err(e) => Err(e).context("cannot inspect configuration destination"),
  }
}

/// Sanitized known URL userinfo. Arbitrary argv and vendor query strings can
/// contain unknown secrets; display is not a universal secret detector.
pub fn safe_text(text: &str, secrets: &[&str]) -> String {
  let mut safe = if let Ok(mut url) = reqwest::Url::parse(text) {
    if !url.username().is_empty() || url.password().is_some() {
      let _ = url.set_username("redacted"); // URL was parsed with a userinfo-capable scheme.
      let _ = url.set_password(None); // Removing userinfo cannot fail on these parsed URLs.
      url.to_string()
    } else {
      text.to_owned()
    }
  } else {
    text.to_owned()
  };
  for secret in secrets.iter().filter(|s| !s.is_empty()) {
    safe = safe.replace(secret, "<redacted>");
  }
  safe
}

/// Render redacted JSON values through YAML's own value model. The workspace
/// enables serde_json arbitrary_precision; serializing its Number directly into
/// another data format exposes the private number representation as a map.
/// NOTICE: serde_json 1.x `arbitrary_precision`, `number.rs::Serialize` emits
/// `$serde_json::private::Number`; JSON text is the supported interchange here.
pub fn yaml(value: &serde_json::Value) -> Result<String> {
  let value: serde_yaml::Value = serde_yaml::from_slice(&serde_json::to_vec(value)?)?;
  Ok(serde_yaml::to_string(&value)?)
}

/// Mask every environment value and schema-known credential key while keeping
/// the shape useful. Unknown free text/argv may still contain user secrets.
pub fn redact(value: &mut serde_json::Value, secrets: &[&str]) {
  match value {
    serde_json::Value::Object(map) => {
      for (key, value) in map {
        if matches!(key.as_str(), "credential" | "token" | "password" | "secret" | "api_key") {
          *value = "<redacted>".into();
        } else if key == "env" || key == "environment" {
          mask_values(value);
        } else {
          redact(value, secrets);
        }
      }
    }
    serde_json::Value::Array(values) => {
      for value in values {
        redact(value, secrets);
      }
    }
    serde_json::Value::String(text) => *text = safe_text(text, secrets),
    _ => {}
  }
}

fn mask_values(value: &mut serde_json::Value) {
  match value {
    serde_json::Value::Object(map) => {
      for value in map.values_mut() {
        mask_values(value);
      }
    }
    serde_json::Value::Array(values) => {
      for value in values {
        mask_values(value);
      }
    }
    _ => *value = "<redacted>".into(),
  }
}

/// Read a bearer/code file without accepting a symlink or Unix group/world
/// access. Ownership follows the running account, independently of its path.
pub fn read_secret(path: &Path) -> Result<String> {
  reject_symlink(path)?;
  let mut options = std::fs::OpenOptions::new();
  options.read(true);
  #[cfg(unix)]
  {
    use std::os::unix::fs::OpenOptionsExt as _;
    options.custom_flags(libc::O_NOFOLLOW);
  }
  let mut file = options.open(path).context("cannot open private credential file")?;
  let metadata = file.metadata().context("cannot inspect private credential file")?;
  anyhow::ensure!(metadata.is_file(), "credential file must be a regular file");
  #[cfg(unix)]
  {
    use std::os::unix::fs::MetadataExt as _;
    anyhow::ensure!(metadata.mode() & 0o077 == 0, "credential file must be private (chmod 600)");
    // SAFETY: geteuid has no preconditions and changes no process state.
    anyhow::ensure!(metadata.uid() == unsafe { libc::geteuid() }, "credential file belongs to another OS account");
  }
  #[cfg(not(unix))]
  anyhow::bail!("private credential file reads on Windows require explicit ACL support");
  let mut text = String::new();
  std::io::Read::read_to_string(&mut file, &mut text).context("cannot read private credential file")?;
  let secret = text.trim_end_matches(['\r', '\n']);
  anyhow::ensure!(!secret.is_empty() && !secret.contains(['\r', '\n', '\0']), "credential file must contain one nonempty credential");
  Ok(secret.to_owned())
}

pub fn invalid(error: needy_config::global::Error, path: &Path) -> anyhow::Error {
  crate::diagnostics::Error::new(error.reason(), error.to_string())
    .fact("source", path.display().to_string())
    .fact("field", error.field())
    .check("Inspect this field with needy config get; preserve the existing configuration while correcting it.")
    .cause(error)
    .into()
}

fn failure(reason: &'static str, summary: &str, path: &Path, cause: anyhow::Error) -> anyhow::Error {
  crate::diagnostics::Error::new(reason, summary)
    .fact("source", path.display().to_string())
    .fact("cause", format!("{cause:#}"))
    .check("Inspect config paths and the file/parent permissions; preserve existing credentials.")
    .cause(cause)
    .into()
}
