//! Actor-to-creating-client acceptance through the public CLI.

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

use super::super::checks::Checks;
use super::super::env::CellEnv;
use super::super::matrix::{Cell, ClientAt};
use super::super::runs;
use super::{Client, STEP};

const EXECUTOR: &str = "project";

/// Client execution after create returns, multiple logical targets, and stop
/// cleanup. Restart survival is owned by the separate lifecycle cells.
pub fn run(env: &mut CellEnv, cell: &Cell, at: ClientAt, checks: &mut Checks) -> Result<()> {
  let mut client = Client::connect(env, at)?;
  let expected =
    client.shell("python3 -c 'import os,json; print(json.dumps(dict(uid=os.getuid(),gid=os.getgid(),home=os.environ[\"HOME\"],platform=os.uname().sysname)))'")?;
  ensure!(expected.ok(), "cannot inspect invoking account: {}", expected.brief());
  let expected: Value = serde_json::from_str(&expected.stdout)?;
  ensure!(expected["uid"] != 0, "client execution acceptance requires a non-root invoking account");
  let mut ids = Vec::new();
  let mut holder_pids = Vec::new();
  let result = (|| -> Result<()> {
    for (index, label) in ["first", "second"].into_iter().enumerate() {
      client.write(&format!("{label}/project-id"), label)?;
      let base = format!("{}/{label}", client.root);
      let profile = profile(cell, &base);
      let text = serde_json::to_string_pretty(&profile)?;
      client.write("profile.yaml", &text)?;
      let out = client.invoke(
        &[
          "run",
          "create",
          "-f",
          "profile.yaml",
          "-t",
          "--env",
          &format!("E2E_CREATE={label}"),
        ],
        true,
      )?;
      ensure!(out.ok(), "client run create failed: {}", out.brief());
      let id = out.stdout.lines().map(str::trim).rfind(|s| !s.is_empty()).context("create printed no run ID")?.to_string();
      ensure!(!id.contains(' '), "create printed an invalid run ID");
      env.runs.push(id.clone());
      ids.push(id.clone());
      runs::wait_marker(env, &id, "E2E-CLIENT-READY")?;
      checks.eq(format!("{label}: profile file remains unchanged after temporary overrides"), text, client.read("profile.yaml")?);
      // Creation has returned. Only the auto-managed holder can service the
      // next routed request; no runner was enrolled or served by the harness.
      client.write(&format!("{label}/trigger"), "go")?;
      let console = runs::wait_marker(env, &id, "E2E-CLIENT-PROBED")?;
      checks.eq(format!("{label}: routed target exit code passes through"), Some("7".into()), console.value("routed-rc"));
      let probe: Value = serde_json::from_str(&console.value("client-probe").context("actor printed no client probe")?)?;
      holder_pids.push(probe["holder"].as_u64().context("probe returned no holder PID")?);
      checks.eq(format!("{label}: command runs on the creating client platform"), expected["platform"].clone(), probe["platform"].clone());
      checks.eq(format!("{label}: native uid is the invoking account"), expected["uid"].clone(), probe["uid"].clone());
      checks.eq(format!("{label}: native gid is the invoking account"), expected["gid"].clone(), probe["gid"].clone());
      checks.eq(format!("{label}: native HOME is the invoking account's HOME"), expected["home"].clone(), probe["home"].clone());
      checks.eq(format!("{label}: cwd maps to this invocation's client workspace"), Value::String(base.clone()), probe["cwd"].clone());
      checks.eq(format!("{label}: commands observe their own project"), Value::String(label.into()), probe["project"].clone());
      checks.eq(format!("{label}: temporary actor environment override takes effect"), Some(label.into()), console.value("create-env"));
      checks.eq(format!("{label}: actor mount reads this client's project"), Some(label.into()), console.value("mounted-project"));
      checks.eq(format!("{label}: routed command writes to this client's project"), label, client.read(&format!("{label}/effect"))?.trim());
      checks.eq(
        format!("{label}: actor writes through the mounted client workspace"),
        "actor",
        client.read(&format!("{label}/actor-effect"))?.trim(),
      );
      let report = runs::report(env, &id)?;
      checks.check(
        format!("{label}: report identifies the logical project executor"),
        runs::routed_to(&report, EXECUTOR) >= 1,
        report["commands"].to_string(),
      );
      client.wait_holders(index + 1)?;
    }
    checks.check("independent creations have distinct execution holders", holder_pids[0] != holder_pids[1], format!("{holder_pids:?}"));
    let out = env.needy(
      &[
        "run",
        "exec",
        &ids[1],
        "--executor",
        "tools",
        "--",
        "python3",
        "-c",
        "import pathlib; print(pathlib.Path(\"project-id\").read_text())",
      ],
      STEP,
    )?;
    checks.check(
      "two logical executors use the same invocation's client capability",
      out.ok() && out.stdout.trim() == "second",
      out.brief(),
    );
    client.wait_holders(2)?;
    let out = env.needy(&["run", "stop", &ids[0]], env.ctx.timeout)?;
    ensure!(out.ok(), "first stop failed: {}", out.brief());
    runs::wait_end(env, &ids[0], env.ctx.timeout)?;
    // The first invocation is already stopped. Keep only still-owned runs
    // in cleanup; stop does not promise idempotence for historical IDs.
    ids.remove(0);
    client.wait_holders(1)?;
    checks.check("stopping the first invocation releases only its holder", true, "one client execution holder remains");
    let out = env.needy(
      &[
        "run",
        "exec",
        &ids[0],
        "--",
        "python3",
        "-c",
        "import pathlib; print(pathlib.Path(\"project-id\").read_text())",
      ],
      STEP,
    )?;
    checks.check(
      "second invocation still executes through its own binding after first stop",
      out.ok() && out.stdout.trim() == "second",
      out.brief(),
    );
    Ok(())
  })();
  // Explicitly stop even on a failed assertion or probe: daemon shutdown no
  // longer owns work lifetime under the approved survival contract.
  for id in &ids {
    let stopped = (|| -> Result<()> {
      let out = env.needy(&["run", "stop", id], env.ctx.timeout)?;
      ensure!(out.ok(), "{}", out.brief());
      runs::wait_end(env, id, env.ctx.timeout)?;
      Ok(())
    })();
    checks.check("explicit stop releases client execution", stopped.is_ok(), stopped.err().map(|e| format!("{e:#}")).unwrap_or_default());
  }
  let cleaned = client.wait_holders(0);
  checks.check(
    "all client execution holder directories disappear after stop",
    cleaned.is_ok(),
    cleaned.err().map(|e| format!("{e:#}")).unwrap_or_default(),
  );
  if !client.callback && checks.passed() && !env.ctx.keep {
    let finished = client.finish(false);
    checks.check("creating-client scratch cleanup succeeds", finished.is_ok(), finished.err().map(|e| format!("{e:#}")).unwrap_or_default());
  }
  if client.callback {
    let finished = super::super::controller::request(&json!({ "op": "finish" }));
    checks.check(
      "local creating-client callback cleanup succeeds",
      finished.is_ok(),
      finished.as_ref().err().map(|e| format!("{e:#}")).unwrap_or_default(),
    );
    // The finish operation consumed its client even on a cleanup error.
    client.callback = false;
  }
  result
}

fn profile(cell: &Cell, source: &str) -> Value {
  let (name, uid) = cell.user.account();
  let probe = "import os,json,pathlib,sys; p=pathlib.Path(\"project-id\").read_text(); pathlib.Path(\"effect\").write_text(p); print(\"E2E client-probe=\"+json.dumps(dict(uid=os.getuid(),gid=os.getgid(),home=os.environ[\"HOME\"],cwd=os.getcwd(),project=p,holder=os.getppid(),platform=os.uname().sysname))); sys.exit(7)";
  let script = format!(
    "echo \"E2E create-env=$E2E_CREATE\"\necho \"E2E mounted-project=$(cat project-id)\"\nprintf actor > actor-effect\necho E2E-CLIENT-READY\nwhile [ ! -f trigger ]; do sleep 1; done\npython3 -c {}\necho \"E2E routed-rc=$?\"\necho E2E-CLIENT-PROBED\nwhile :; do sleep 1; done",
    shell_words::quote(probe),
  );
  let actor = super::super::scenarios::actor(cell, &script);
  let mut profile = json!({
    "user": { "name": name, "uid": uid, "gid": uid },
    "actor": actor,
    "executors": {
      "project": { "driver": "native", "runner": { "client": true } },
      "tools": { "driver": "native", "runner": { "client": true } },
    },
    "commands": [{ "match": "python3 **", "to": EXECUTOR }],
  });
  profile["actor"]["workspace"] = json!({ "path": "~/workspace", "from": format!("executor:{EXECUTOR}:{source}"), "writable": true });
  // Both logical executors use this invocation's client; routing tools must
  // not acquire a second account or publish the holder as a shared runner.
  profile["executors"]["tools"]["workspace"] = Value::String(source.into());
  profile
}
