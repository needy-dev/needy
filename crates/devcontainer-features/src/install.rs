//! Dev Container feature `install.sh` environment and POSIX wrapper script
//! generation.
//!
//! Reference: https://containers.dev/implementors/features/#user-env-var (retrieved 2026-11-21)

use std::collections::BTreeMap;

/// The target remote user for whom a Dev Container feature is installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallUser {
  pub name: String,
  pub uid: u32,
  pub gid: u32,
  /// Explicit home directory; defaults to `/root` for root (uid 0, name
  /// `"root"`) and `/home/<name>` otherwise.
  pub home: Option<String>,
}

impl InstallUser {
  /// Returns the effective home directory for this user.
  pub fn home_dir(&self) -> String {
    if let Some(home) = &self.home {
      return home.clone();
    }
    if self.uid == 0 && self.name == "root" {
      "/root".to_string()
    } else {
      format!("/home/{}", self.name)
    }
  }
}

fn sh_quote(value: &str) -> String {
  format!("'{}'", value.replace('\'', "'\\''"))
}

/// Builds the environment passed to a Dev Container feature's `install.sh`
/// session: the sanitized option variables plus `_REMOTE_USER`,
/// `_REMOTE_USER_HOME`, `_CONTAINER_USER`, and `_CONTAINER_USER_HOME`.
// Reference: https://containers.dev/implementors/features/#user-env-var (retrieved 2026-11-21)
pub fn feature_install_env(user: &InstallUser, options: &BTreeMap<String, String>) -> BTreeMap<String, String> {
  let mut env = options.clone();
  env.insert("_REMOTE_USER".into(), user.name.clone());
  env.insert("_REMOTE_USER_HOME".into(), user.home_dir());
  env.insert("_CONTAINER_USER".into(), "root".into());
  env.insert("_CONTAINER_USER_HOME".into(), "/root".into());
  env
}

/// Builds a POSIX `sh -c` wrapper script that ensures `user` exists, writes
/// and sources `devcontainer-features.env` inside `staging_dir`, executes
/// `./install.sh` directly (honoring its `#!` shebang), and removes
/// `staging_dir` afterwards so staging files do not leak into the committed
/// layer.
pub fn feature_install_script(user: &InstallUser, options: &BTreeMap<String, String>, staging_dir: &str) -> String {
  let home = user.home_dir();
  let q_name = sh_quote(&user.name);
  let q_home = sh_quote(&home);
  let q_staging = sh_quote(staging_dir);
  let mut env_file = String::new();
  for (key, value) in options {
    env_file.push_str(key);
    env_file.push('=');
    env_file.push_str(&sh_quote(value));
    env_file.push('\n');
  }
  let q_env_file = sh_quote(&env_file);
  format!(
    r#"set -eu
if [ {uid} -ne 0 ]; then
  if ! grep -q "^{raw_name}:" /etc/group 2>/dev/null; then
    if command -v groupadd >/dev/null 2>&1; then
      groupadd -g {gid} {q_name} 2>/dev/null || true
    elif command -v addgroup >/dev/null 2>&1; then
      addgroup -g {gid} {q_name} 2>/dev/null || true
    else
      printf '%s:x:%s:\n' {q_name} {gid} >> /etc/group
    fi
  fi
  if ! id -u {q_name} >/dev/null 2>&1; then
    if command -v useradd >/dev/null 2>&1; then
      useradd -m -u {uid} -g {gid} -d {q_home} -s /bin/sh {q_name} 2>/dev/null || true
    elif command -v adduser >/dev/null 2>&1; then
      adduser -D -u {uid} -G {q_name} -h {q_home} -s /bin/sh {q_name} 2>/dev/null || true
    else
      printf '%s:x:%s:%s::%s:/bin/sh\n' {q_name} {uid} {gid} {q_home} >> /etc/passwd
    fi
  fi
  mkdir -p {q_home}
  chown {uid}:{gid} {q_home}
fi
cd {q_staging}
printf '%s' {q_env_file} > devcontainer-features.env
set -a
. ./devcontainer-features.env
set +a
chmod +x ./install.sh
status=0
./install.sh || status=$?
cd /
rm -rf {q_staging}
exit "$status"
"#,
    uid = user.uid,
    gid = user.gid,
    raw_name = user.name,
    q_name = q_name,
    q_home = q_home,
    q_staging = q_staging,
    q_env_file = q_env_file,
  )
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn install_env_sets_remote_and_container_user_vars() {
    let user = InstallUser {
      name: "dev".into(),
      uid: 1001,
      gid: 1001,
      home: None,
    };
    let opts = BTreeMap::from([("VERSION".into(), "22".into())]);
    let env = feature_install_env(&user, &opts);
    assert_eq!(env["_REMOTE_USER"], "dev");
    assert_eq!(env["_REMOTE_USER_HOME"], "/home/dev");
    assert_eq!(env["_CONTAINER_USER"], "root");
    assert_eq!(env["_CONTAINER_USER_HOME"], "/root");
    assert_eq!(env["VERSION"], "22");

    let root = InstallUser {
      name: "root".into(),
      uid: 0,
      gid: 0,
      home: None,
    };
    assert_eq!(feature_install_env(&root, &BTreeMap::new())["_REMOTE_USER_HOME"], "/root");
  }

  #[test]
  fn install_script_writes_env_honors_shebang_and_cleans_up() {
    let user = InstallUser {
      name: "agent".into(),
      uid: 1000,
      gid: 1000,
      home: None,
    };
    let opts = BTreeMap::from([
      ("VERSION".into(), "22".into()),
      ("MSG".into(), "it's ok".into()),
    ]);
    let script = feature_install_script(&user, &opts, "/tmp/needy-feature");
    assert!(script.contains("devcontainer-features.env"), "{script}");
    assert!(script.contains("\n./install.sh || status=$?\n"), "{script}");
    assert!(!script.contains("sh ./install.sh"), "{script}");
    assert!(script.contains("rm -rf '/tmp/needy-feature'"), "{script}");
    assert!(script.contains("VERSION='\\''22'\\''"), "{script}");
  }
}
