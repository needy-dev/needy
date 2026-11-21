//! The `image` scenario: `run:` layers, `env` / `PATH`, `image build`,
//! `image edit` (with lockfile commit and `run:` snippet), and `image rollback`
//! on VM and container drivers.

use std::time::Duration;

use anyhow::{Result, bail};
use serde_json::json;

use super::checks::Checks;
use super::env::CellEnv;
use super::matrix::{Cell, Driver, End};
use super::runs::{self, check_ended, end_run};

const BUILD: Duration = Duration::from_secs(180);
const STEP: Duration = Duration::from_secs(120);

pub fn run(env: &mut CellEnv, cell: &Cell, checks: &mut Checks) -> Result<()> {
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
  checks.check("image build prints a snapshot digest", built_snapshot.starts_with("sha256:"), format!("{built_snapshot:?}"));

  let rebuilt = env.needy(&["image", "build", "--profile", &profile_str], BUILD)?;
  checks.check("second image build succeeds", rebuilt.ok(), rebuilt.brief());
  let rebuilt_snapshot = rebuilt.stdout.lines().map(str::trim).rfind(|l| !l.is_empty()).unwrap_or_default().to_string();
  checks.eq("second image build hits cache with the same snapshot digest", built_snapshot.clone(), rebuilt_snapshot);

  // 2. First run: declared `run:` layer and image `env`/`PATH` are active.
  let run1 = runs::create(env, &profile, &[])?;
  let (row1, console1) = end_run(env, &run1, End::Exit, checks, env.ctx.timeout)?;
  check_ended(checks, End::Exit, &row1, 0);
  checks.value(&console1, "tool-path", "/opt/e2e-bin/e2e-layer-tool");
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
  checks.check(
    "post-edit run records a new snapshot digest",
    edited_snapshot.starts_with("sha256:") && edited_snapshot != built_snapshot,
    format!("before={built_snapshot}, after={edited_snapshot}"),
  );

  // 5. Roll back the commit and verify the lockfile is cleared.
  let rolled = env.needy(&["image", "rollback", "--profile", &profile_str], STEP)?;
  checks.check("image rollback succeeds", rolled.ok(), rolled.brief());
  checks.check("image rollback clears the lockfile", !lockfile.exists(), format!("{} still exists", lockfile.display()));

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
