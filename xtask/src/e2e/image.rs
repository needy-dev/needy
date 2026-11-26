//! The `image` scenario: `run:` layers, `env` / `PATH`, `image build`,
//! `image edit` (with lockfile commit and `run:` snippet), and `image rollback`
//! on VM and container drivers.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use serde::Deserialize;
use serde_json::json;

use super::checks::Checks;
use super::env::CellEnv;
use super::matrix::{Cell, Driver, End};
use super::proc;
use super::runs::{self, check_ended, end_run};

const BUILD: Duration = Duration::from_secs(180);
const FEATURE_BUILD: Duration = Duration::from_secs(300);
const STEP: Duration = Duration::from_secs(120);

/// Converts a `sha256:<hex>` snapshot digest into its container image tag.
fn snapshot_tag(digest: &str) -> Option<String> {
  let hex = digest.strip_prefix("sha256:")?;
  (!hex.is_empty()).then(|| format!("needy-snapshot:{hex}"))
}

/// The built-in feature every image build pins without the image naming it.
// Source: `crates/core/src/image/mod.rs`, `BUILTIN_SSHFS`.
const IMPLICIT_FEATURE: &str = "builtin:sshfs";

/// What the harness reads of an image's `<image>.lock`.
// Source: `crates/cli/src/commands/image/lockfile.rs`, `Lockfile` (the CLI
// owns the file; unknown keys fail here too, so a format change shows).
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Lock {
  #[serde(default)]
  commit: Option<serde_yaml::Value>,
  #[serde(default)]
  features: BTreeMap<String, String>,
}

/// `path` parsed, or an empty lock when the CLI removed the file (it does
/// when nothing is left to record).
fn read_lock(path: &Path) -> Result<Lock> {
  match std::fs::read_to_string(path) {
    Ok(text) => serde_yaml::from_str(&text).with_context(|| format!("cannot parse {}: {text}", path.display())),
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Lock::default()),
    Err(error) => Err(error).with_context(|| format!("cannot read {}", path.display())),
  }
}

/// Removes `needy-snapshot:<hex>` container images materialized during an
/// `image` or `image-feature` cell when the cell finishes or returns early.
struct SnapshotCleanup {
  driver: Driver,
  tags: Vec<String>,
}

impl SnapshotCleanup {
  fn new(driver: Driver) -> Self {
    Self {
      driver,
      tags: Vec::new(),
    }
  }

  fn track(&mut self, digest: &str) {
    if let Some(tag) = snapshot_tag(digest)
      && !self.tags.contains(&tag)
    {
      self.tags.push(tag);
    }
  }
}

impl Drop for SnapshotCleanup {
  fn drop(&mut self) {
    for tag in &self.tags {
      match self.driver {
        Driver::Container(_) => {
          // Best-effort teardown; `host::clean` also prunes `needy-snapshot:*`.
          let _ = proc::simple("docker", &["image", "rm", "-f", tag], Duration::from_secs(30));
        }
        Driver::AppleContainer => {
          // Best-effort teardown; `host::clean` also prunes `needy-snapshot:*`.
          let _ = proc::simple("container", &["image", "delete", tag], Duration::from_secs(30));
        }
        Driver::Firecracker | Driver::Vz | Driver::Host => {}
      }
    }
  }
}

pub fn run(env: &mut CellEnv, cell: &Cell, checks: &mut Checks) -> Result<()> {
  let mut cleanup = SnapshotCleanup::new(cell.driver);
  let (name, id) = cell.user.account();
  let root_line = match cell.driver {
    Driver::Container(_) | Driver::AppleContainer => format!("root: {}\n", env.ctx.host.image_names.actor),
    Driver::Firecracker | Driver::Vz | Driver::Host => String::new(),
  };
  let image_yaml = format!(
    r#"{root_line}layers:
  - run: |
      mkdir -p /opt/e2e-bin
      printf '#!/bin/sh\necho "e2e-layer-ok"\n' > /opt/e2e-bin/e2e-layer-tool
      chmod 755 /opt/e2e-bin/e2e-layer-tool
      echo "declared-layer" > /etc/e2e-layer.txt
env:
  PATH: "/opt/e2e-bin:${{PATH}}"
  E2E_LAYER_ENV: "from-image"
"#
  );
  let image_path = env.file("test-image.yaml", &image_yaml, 0o644)?;
  let lockfile = image_path.with_file_name("test-image.yaml.lock");

  let script = r#"
echo "E2E tool-path=$(command -v e2e-layer-tool 2>/dev/null || echo absent)"
echo "E2E tool=$(e2e-layer-tool 2>/dev/null || echo absent)"
echo "E2E layer-file=$(cat /etc/e2e-layer.txt 2>/dev/null || echo absent)"
echo "E2E layer-env=${E2E_LAYER_ENV:-absent}"
echo "E2E edited-file=$(cat /etc/e2e-edited.txt 2>/dev/null || echo absent)"
echo "E2E edit-tool=$(e2e-edit-tool 2>/dev/null || echo absent)"
exit 0
"#;

  let mut actor = json!({
    "driver": cell.driver.profile_name(),
    "image": "./test-image.yaml",
    "command": ["/bin/sh", "-c", script],
  });
  if let Driver::Container(oci) = cell.driver {
    actor["container"] = json!({ "oci_runtime": oci.name() });
  }
  let profile = env.profile(&json!({
    "user": { "name": name, "uid": id, "gid": id },
    "actor": actor,
  }))?;
  let profile_str = profile.display().to_string();

  // 1. Build the declared image definition and verify caching.
  let built = env.needy(&["image", "build", "--profile", &profile_str], BUILD)?;
  if !built.ok() {
    bail!("image build failed: {}", built.brief());
  }
  let built_snapshot = built.stdout.lines().map(str::trim).rfind(|l| !l.is_empty()).unwrap_or_default().to_string();
  cleanup.track(&built_snapshot);
  checks.check("image build prints a snapshot digest", built_snapshot.starts_with("sha256:"), format!("{built_snapshot:?}"));

  let rebuilt = env.needy(&["image", "build", "--profile", &profile_str], BUILD)?;
  checks.check("second image build succeeds", rebuilt.ok(), rebuilt.brief());
  let rebuilt_snapshot = rebuilt.stdout.lines().map(str::trim).rfind(|l| !l.is_empty()).unwrap_or_default().to_string();
  checks.eq("second image build hits cache with the same snapshot digest", built_snapshot.clone(), rebuilt_snapshot);

  // 2. First run: declared `run:` layer and image `env`/`PATH` are active.
  let run1 = runs::create(env, &profile, &[])?;
  let (row1, console1) = end_run(env, &run1, End::Exit, checks, env.ctx.timeout)?;
  check_ended(checks, End::Exit, &row1, 0);
  // Container drivers capture execs via PATH trampolines under `/run/needy/shims`
  // prepended ahead of `image_path`; VM drivers capture in the kernel via
  // `binfmt_needy`, so `command -v` resolves directly to `/opt/e2e-bin`.
  let expected_tool_path = match cell.driver {
    Driver::Container(_) | Driver::AppleContainer => "/run/needy/shims/e2e-layer-tool",
    Driver::Firecracker | Driver::Vz | Driver::Host => "/opt/e2e-bin/e2e-layer-tool",
  };
  checks.value(&console1, "tool-path", expected_tool_path);
  checks.value(&console1, "tool", "e2e-layer-ok");
  checks.value(&console1, "layer-file", "declared-layer");
  checks.value(&console1, "layer-env", "from-image");
  checks.value(&console1, "edited-file", "absent");
  checks.value(&console1, "edit-tool", "absent");

  let report1 = runs::report(env, &run1)?;
  checks.eq("run report records the built snapshot digest", Some(built_snapshot.as_str()), report1["snapshot"].as_str());

  // 3. Interactive `image edit`: create files, exit cleanly, commit to lockfile.
  let mut term = env.term(&["image", "edit", "--profile", &profile_str])?;
  term.expect("# ", BUILD)?;
  let mark = term.mark();
  term.send(
    "sh -c 'echo edited-layer > /etc/e2e-edited.txt && printf \"#!/bin/sh\\necho e2e-edit-ok\\n\" > /usr/local/bin/e2e-edit-tool && chmod 755 /usr/local/bin/e2e-edit-tool' && echo E2E-EDIT-DONE\r",
  )?;
  term.expect_from(mark, "E2E-EDIT-DONE", STEP)?;
  term.send("exit 0\r")?;
  let edit_code = term.wait(BUILD)?;
  let edit_transcript = term.transcript();
  checks.eq("image edit exits 0 on clean commit", 0, edit_code);
  checks.check("image edit reports the committed layer", edit_transcript.contains("image edit: committed sha256:"), edit_transcript.clone());
  checks.check("image edit prints a pasteable `run:` snippet", edit_transcript.contains("- run: |"), edit_transcript.clone());
  checks.check("image edit snippet records executed commands", edit_transcript.contains("chmod"), edit_transcript);

  let lock_text = std::fs::read_to_string(&lockfile).unwrap_or_default();
  checks.check(
    "image edit writes commit to the lockfile",
    lock_text.contains("commit:") && lock_text.contains("layer: sha256:") && lock_text.contains("base: sha256:"),
    lock_text,
  );

  // 4. Second run: committed edit is visible alongside the declared `run:` layer.
  let run2 = runs::create(env, &profile, &[])?;
  let (row2, console2) = end_run(env, &run2, End::Exit, checks, env.ctx.timeout)?;
  check_ended(checks, End::Exit, &row2, 0);
  checks.value(&console2, "tool", "e2e-layer-ok");
  checks.value(&console2, "layer-file", "declared-layer");
  checks.value(&console2, "layer-env", "from-image");
  checks.value(&console2, "edited-file", "edited-layer");
  checks.value(&console2, "edit-tool", "e2e-edit-ok");

  let report2 = runs::report(env, &run2)?;
  let edited_snapshot = report2["snapshot"].as_str().unwrap_or_default().to_string();
  cleanup.track(&edited_snapshot);
  checks.check(
    "post-edit run records a new snapshot digest",
    edited_snapshot.starts_with("sha256:") && edited_snapshot != built_snapshot,
    format!("before={built_snapshot}, after={edited_snapshot}"),
  );

  // 5. Roll back the commit and verify the lockfile no longer records it.
  // NOTICE: rollback removes the commit, not the feature pins. A
  // `linux-container` build pins the implicit `builtin:sshfs` (since #47 and
  // #51), so its lockfile stays with that pin; a VM build pins none, and the
  // CLI removes the then-empty file. This test image names no feature, so
  // the implicit pin is the only one that may remain, unchanged.
  let pinned = read_lock(&lockfile)?.features;
  let rolled = env.needy(&["image", "rollback", "--profile", &profile_str], STEP)?;
  checks.check("image rollback succeeds", rolled.ok(), rolled.brief());
  let lock = read_lock(&lockfile)?;
  checks.check("image rollback removes the commit from the lockfile", lock.commit.is_none(), format!("{lock:?}"));
  checks.check(
    "image rollback keeps only the implicit feature pins, unchanged",
    lock.features.keys().all(|feature| feature == IMPLICIT_FEATURE) && lock.features == pinned,
    format!("before={pinned:?}, after={:?}", lock.features),
  );

  // 6. Third run: committed edit is gone, declared `run:` layer remains.
  let run3 = runs::create(env, &profile, &[])?;
  let (row3, console3) = end_run(env, &run3, End::Exit, checks, env.ctx.timeout)?;
  check_ended(checks, End::Exit, &row3, 0);
  checks.value(&console3, "tool", "e2e-layer-ok");
  checks.value(&console3, "layer-file", "declared-layer");
  checks.value(&console3, "layer-env", "from-image");
  checks.value(&console3, "edited-file", "absent");
  checks.value(&console3, "edit-tool", "absent");

  let report3 = runs::report(env, &run3)?;
  checks.eq("post-rollback run restores the original snapshot digest", Some(built_snapshot.as_str()), report3["snapshot"].as_str());

  Ok(())
}

/// The `image-feature` scenario: fetches a published Dev Container feature
/// from an OCI registry, builds it in a session with `with:` options, pins its
/// digest in `<image>.lock`, verifies cached rebuild, and boots a run to check
/// the installed tool on `PATH` and `run report`.
pub fn run_feature(env: &mut CellEnv, cell: &Cell, checks: &mut Checks) -> Result<()> {
  let mut cleanup = SnapshotCleanup::new(cell.driver);
  let (name, id) = cell.user.account();
  let root_line = match cell.driver {
    Driver::Container(_) | Driver::AppleContainer => format!("root: {}\n", env.ctx.host.image_names.actor),
    Driver::Firecracker | Driver::Vz | Driver::Host => String::new(),
  };
  let image_yaml = format!(
    r#"{root_line}layers:
  - feature: ghcr.io/devcontainers/features/github-cli:1
    with:
      installDirectlyFromGitHubRelease: true
"#
  );
  let image_path = env.file("test-feature-image.yaml", &image_yaml, 0o644)?;
  let lockfile = image_path.with_file_name("test-feature-image.yaml.lock");

  let script = r#"
echo "E2E gh-path=$(command -v gh 2>/dev/null || echo absent)"
echo "E2E gh-version=$(gh --version 2>/dev/null | head -n 1 || echo absent)"
exit 0
"#;

  let mut actor = json!({
    "driver": cell.driver.profile_name(),
    "image": "./test-feature-image.yaml",
    "command": ["/bin/sh", "-c", script],
  });
  if let Driver::Container(oci) = cell.driver {
    actor["container"] = json!({ "oci_runtime": oci.name() });
  }
  let profile = env.profile(&json!({
    "user": { "name": name, "uid": id, "gid": id },
    "actor": actor,
  }))?;
  let profile_str = profile.display().to_string();

  // 1. Build the feature image: pulls from OCI, runs `install.sh`, pins lockfile.
  let built = env.needy(&["image", "build", "--profile", &profile_str], FEATURE_BUILD)?;
  if !built.ok() {
    bail!("feature image build failed: {}", built.brief());
  }
  let built_snapshot = built.stdout.lines().map(str::trim).rfind(|l| !l.is_empty()).unwrap_or_default().to_string();
  cleanup.track(&built_snapshot);
  checks.check("feature image build prints a snapshot digest", built_snapshot.starts_with("sha256:"), format!("{built_snapshot:?}"));

  let lock_text = std::fs::read_to_string(&lockfile).unwrap_or_default();
  checks.check(
    "feature image build writes pinned digest to lockfile",
    lock_text.contains("features:") && lock_text.contains("ghcr.io/devcontainers/features/github-cli: sha256:"),
    lock_text,
  );

  // 2. Second build hits cache with the same snapshot digest.
  let rebuilt = env.needy(&["image", "build", "--profile", &profile_str], BUILD)?;
  checks.check("second feature image build succeeds", rebuilt.ok(), rebuilt.brief());
  let rebuilt_snapshot = rebuilt.stdout.lines().map(str::trim).rfind(|l| !l.is_empty()).unwrap_or_default().to_string();
  checks.eq("second feature image build hits cache with the same snapshot digest", built_snapshot.clone(), rebuilt_snapshot);

  // 3. Boot the snapshot and verify the installed tool on `PATH` and `run report`.
  let run_id = runs::create(env, &profile, &[])?;
  let (row, console) = end_run(env, &run_id, End::Exit, checks, env.ctx.timeout)?;
  check_ended(checks, End::Exit, &row, 0);

  let expected_gh_path = match cell.driver {
    Driver::Container(_) | Driver::AppleContainer => "/run/needy/shims/gh",
    Driver::Firecracker | Driver::Vz | Driver::Host => "/usr/bin/gh",
  };
  checks.value(&console, "gh-path", expected_gh_path);
  let gh_version = console.value("gh-version").unwrap_or_default();
  checks.check("gh --version succeeds", gh_version.starts_with("gh version "), format!("{gh_version:?}"));

  let report = runs::report(env, &run_id)?;
  checks.eq("run report records the built feature snapshot digest", Some(built_snapshot.as_str()), report["snapshot"].as_str());

  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn snapshot_tag_formats_sha256_digests_and_rejects_others() {
    assert_eq!(snapshot_tag("sha256:0123456789abcdef"), Some("needy-snapshot:0123456789abcdef".to_string()));
    assert_eq!(snapshot_tag("sha256:"), None);
    assert_eq!(snapshot_tag("not-a-digest"), None);
  }

  #[test]
  fn read_lock_reads_a_feature_pin_without_a_commit_and_a_missing_file_as_empty() {
    let dir = std::env::temp_dir().join(format!("xtask-lock-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("image.yaml.lock");
    // The shape the CLI writes after a rollback of a `linux-container` build.
    std::fs::write(&path, "# Managed by needy. Do not edit by hand.\nfeatures:\n  builtin:sshfs: sha256:0123\n").unwrap();
    let lock = read_lock(&path).unwrap();
    assert!(lock.commit.is_none());
    assert_eq!(lock.features.keys().collect::<Vec<_>>(), [IMPLICIT_FEATURE]);
    std::fs::write(&path, "commit:\n  layer: sha256:01\n  base: sha256:02\n").unwrap();
    assert!(read_lock(&path).unwrap().commit.is_some());
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(read_lock(&path).unwrap().commit.is_none());
  }
}
