//! A registry host's CA files in containers-style `certs.d` directories:
//! `<root>/<host[:port]>/*.crt`, PEM, each a CA the host's certificate may
//! chain to, trusted in addition to the system's store.
//!
//! As containers/image does, the first root that has a directory for the
//! host is used and the others are not read, so a user's directory can
//! replace the system's for a host.
// Source: https://github.com/containers/image/blob/v5.36.0/docker/docker_client.go#L67-L73
// (`homeCertDir`, `perHostCertDirs`) and #L149-L184 (`dockerCertDir`: the
// first existing directory wins).
// Reference: https://github.com/containers/image/blob/v5.36.0/docs/containers-certs.d.5.md
// NOTICE: containers/image reads `$HOME/.config/containers/certs.d`; needy
// reads `$XDG_CONFIG_HOME/containers/certs.d` when `XDG_CONFIG_HOME` is set,
// which is the same directory when it is not. `/etc/docker/certs.d` is read
// last because containers/image reads it too, so a CA already set up for
// Docker works without copying it.
// NOTICE: a host with a port has two directory names, tried in this order:
// containerd's `host_port_` spelling, then `host:port` as Docker and
// containers/image name it (`hostport`, without the colon, on Windows, whose
// file names cannot hold `:`). containerd's `_default` directory is not read.
// Source: https://github.com/containerd/containerd/blob/927de108c3f3bb39c3aeab011caf05a64fac1caf/core/remotes/docker/config/hosts.go#L305-L312
// (`hostDirectory`), https://github.com/containerd/containerd/blob/927de108c3f3bb39c3aeab011caf05a64fac1caf/core/remotes/docker/config/config_unix.go#L26-L35
// and https://github.com/containerd/containerd/blob/927de108c3f3bb39c3aeab011caf05a64fac1caf/core/remotes/docker/config/config_windows.go#L25-L34
// (`hostPaths`).

use std::path::{Path, PathBuf};

use crate::{Error, Result};

/// The standard `certs.d` roots, in the order they are tried: the user's
/// (`$XDG_CONFIG_HOME/containers/certs.d`, or `~/.config/containers/certs.d`),
/// then `/etc/containers/certs.d`, then `/etc/docker/certs.d`.
pub fn default_roots() -> Vec<PathBuf> {
  let user = std::env::var_os("XDG_CONFIG_HOME")
    .map(PathBuf::from)
    .filter(|dir| dir.is_absolute())
    .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".config")))
    .map(|config| config.join("containers").join("certs.d"));
  // NOTICE: platform-defined system paths (containers-certs.d(5),
  // https://docs.docker.com/engine/security/certificates/).
  user
    .into_iter()
    .chain([
      PathBuf::from("/etc/containers/certs.d"),
      PathBuf::from("/etc/docker/certs.d"),
    ])
    .collect()
}

/// The directory names a root may hold for `host`, most specific first: for
/// `host:port`, `host_port_` and then `host:port` (`hostport` on Windows);
/// for a host without a port, the host.
pub fn host_directories(host: &str) -> Vec<String> {
  let colonless = |name: &str| {
    if cfg!(windows) {
      name.replace(':', "")
    } else {
      name.to_string()
    }
  };
  match host.rfind(':') {
    Some(idx) if idx > 0 => vec![
      format!("{}_{}_", &host[..idx], &host[idx + 1..]),
      colonless(host),
    ],
    _ => vec![host.to_string()],
  }
}

/// The PEM CA files for `host` from the first root that has a directory for
/// it, in file name order; none when no root has one.
pub fn for_host(roots: &[PathBuf], host: &str) -> Result<Vec<Vec<u8>>> {
  let error = |path: &Path| {
    let path = path.to_path_buf();
    move |source| Error::Certificates {
      host: host.to_string(),
      path,
      source,
    }
  };
  for root in roots {
    let Some(dir) = host_directories(host).into_iter().map(|name| root.join(name)).find(|dir| dir.is_dir()) else {
      continue;
    };
    let mut files = Vec::new();
    for entry in std::fs::read_dir(&dir).map_err(error(&dir))? {
      let path = entry.map_err(error(&dir))?.path();
      match path.extension().and_then(|e| e.to_str()) {
        Some("crt") => files.push(path),
        // TODO(registry-mtls): see the crate's notice; a client pair is
        // named here so an operator sees why it is not used.
        Some("cert" | "key") => {
          tracing::debug!("{} is a client certificate or key, which needy does not present yet", path.display());
        }
        _ => {}
      }
    }
    files.sort();
    return files.iter().map(|path| std::fs::read(path).map_err(error(path))).collect();
  }
  Ok(Vec::new())
}
