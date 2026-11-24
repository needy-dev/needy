//! Three-process end-to-end cell for the remote Firecracker actor runner
//! (`needy runner serve --roles actor --ephemeral`) + Node Authorizer and
//! K8s-friendly runner ergonomics (Phase 1):
//!
//! 1. **Control daemon (no KVM)**: `needy daemon serve --listen tcp://...
//!    --gateway-listen tcp://... --disable-host-files --runners-file ...
//!    --kvm <nonexistent>`
//! 2. **Static-credential executor runner + activity hook**: `needy runner serve
//!    --address tcp://... --credential-env NEEDY_RUNNER_TOKEN --roles executor
//!    --labels role=exec-e2e --activity-hook <script>`
//! 3. **Ephemeral remote Firecracker actor runner (with KVM)**: `needy runner
//!    serve --address tcp://... --credential-env NEEDY_ACTOR_RUNNER_TOKEN
//!    --roles actor --labels role=actor-e2e --ephemeral --gateway 127.0.0.1:<gw_port>`

use std::fs::File;
use std::path::Path;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use needy_api_client::Client;
use needy_api_client::proto::v1::GetRunRequest;
use nix::sys::signal::Signal;
use serde_json::json;

use super::checks::Checks;
use super::env::CellEnv;
use super::matrix::{Cell, End};
use super::proc;
use super::runs::{self, check_ended};

const EXEC_RUNNER_NAME: &str = "e2e-exec";
const EXEC_RUNNER_TOKEN: &str = "needy-runner.e2e-exec.0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const ACTOR_RUNNER_NAME: &str = "e2e-actor";
const ACTOR_RUNNER_TOKEN: &str = "needy-runner.e2e-actor.fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";
const EXECUTOR_NAME: &str = "x";

/// Guards a child process spawned outside `CellEnv::runners` so an early error
/// still sends `SIGTERM` and reaps it before the cell ends.
struct ChildGuard {
  child: Option<Child>,
}

impl ChildGuard {
  fn new(child: Child) -> Self {
    Self { child: Some(child) }
  }

  fn wait_exit(&mut self, timeout: Duration) -> Result<std::process::ExitStatus> {
    let child = self.child.as_mut().context("ephemeral actor runner already reaped")?;
    let deadline = Instant::now() + timeout;
    loop {
      if let Some(status) = child.try_wait()? {
        self.child = None;
        return Ok(status);
      }
      if Instant::now() >= deadline {
        let pid = child.id() as i32;
        proc::signal(pid, Signal::SIGTERM);
        std::thread::sleep(Duration::from_secs(2));
        let _ = child.kill();
        let _ = child.wait();
        self.child = None;
        bail!("ephemeral actor runner did not exit within {timeout:?}");
      }
      std::thread::sleep(Duration::from_millis(100));
    }
  }
}

impl Drop for ChildGuard {
  fn drop(&mut self) {
    if let Some(mut child) = self.child.take() {
      let pid = child.id() as i32;
      proc::signal(pid, Signal::SIGTERM);
      let deadline = Instant::now() + Duration::from_secs(5);
      while matches!(child.try_wait(), Ok(None)) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
      }
      let _ = child.kill();
      let _ = child.wait();
    }
  }
}

pub fn run(env: &mut CellEnv, cell: &Cell, checks: &mut Checks) -> Result<()> {
  let ctrl_port = super::transport::free_port()?;
  let gw_port = super::transport::free_port()?;
  let ctrl_addr = format!("tcp://127.0.0.1:{ctrl_port}");
  let gw_listen = format!("tcp://127.0.0.1:{gw_port}");
  let gw_addr = format!("127.0.0.1:{gw_port}");

  // Static runner manifest loaded by Process 1 via `--runners-file` for both
  // the executor runner and the ephemeral actor runner.
  let runners_yaml = format!(
    "runners:\n\
       - name: {EXEC_RUNNER_NAME}\n\
         credential: \"{EXEC_RUNNER_TOKEN}\"\n\
         roles:\n\
           - executor\n\
         labels:\n\
           role: exec-e2e\n\
       - name: {ACTOR_RUNNER_NAME}\n\
         credential: \"{ACTOR_RUNNER_TOKEN}\"\n\
         roles:\n\
           - actor\n\
         labels:\n\
           role: actor-e2e\n"
  );
  let runners_file = env.file("static-runners.yaml", &runners_yaml, 0o600)?;

  // Activity hook script invoked asynchronously by Process 2 on routed exec/SFTP.
  let hook_file = env.path("activity-hook.log");
  let hook_script = env.file("activity-hook.sh", &format!("#!/bin/sh\necho tick >> \"{}\"\n", hook_file.display()), 0o755)?;

  // Workspace seeded on the runner side, plus a runner-side marker file for routed exec.
  let ws = env.mkdir("ws", 0o755)?;
  env.file("ws/seed.txt", "from-runner-ws\n", 0o644)?;
  let marker = env.file("runner-marker", "present\n", 0o644)?;

  // Short private work directory for Firecracker sockets (`sun_path` + 0700 privacy invariant).
  let actor_work = env.mkdir("aw", 0o700)?;

  // Process 1: Control daemon WITHOUT KVM, with `--disable-host-files`,
  // `--runners-file`, and `--gateway-listen`.
  let no_kvm = env.path("no-such-kvm");
  let daemon_args = vec![
    "--listen".to_string(),
    ctrl_addr.clone(),
    "--gateway-listen".into(),
    gw_listen,
    "--disable-host-files".into(),
    "--runners-file".into(),
    runners_file.display().to_string(),
    "--kvm".into(),
    no_kvm.display().to_string(),
    "--local-runner".into(),
    "e2e-ctrl".into(),
    "-v".into(),
  ];
  env.start_daemon_custom(&daemon_args)?;
  super::invariants::sockets(env, checks, "at daemon start");
  super::invariants::discovery(env, checks);

  // Process 2: Static-credential executor runner with `--credential-env`
  // (read-only; never writes `runner.json`) and `--activity-hook`.
  let runner_log = File::options().create(true).append(true).open(env.dir.join("runner.log"))?;
  let runner_state = env.path("rs");
  let mut runner_cmd = env.needy_command(&[
    "runner",
    "serve",
    "--address",
    &ctrl_addr,
    "--credential-env",
    "NEEDY_RUNNER_TOKEN",
    "--roles",
    "executor",
    "--labels",
    "role=exec-e2e",
    "--activity-hook",
    &hook_script.display().to_string(),
    "--activity-hook-interval",
    "1s",
    "-v",
  ]);
  runner_cmd
    .env("NEEDY_STATE_DIR", &runner_state)
    .env("NEEDY_RUNTIME_DIR", env.path("rr"))
    .env("NEEDY_CONFIG_DIR", env.path("rc"))
    .env("NEEDY_RUNNER_TOKEN", EXEC_RUNNER_TOKEN)
    .stdin(Stdio::null())
    .stdout(runner_log.try_clone()?)
    .stderr(runner_log);
  let runner_child = runner_cmd.spawn().context("cannot start static-credential executor runner")?;
  env.add_runner(runner_child);

  // Process 3: Ephemeral remote Firecracker actor runner (with KVM).
  let actor_log = File::options().create(true).append(true).open(env.dir.join("actor.log"))?;
  let actor_state = env.path("as");
  let mut actor_args = vec![
    "runner".to_string(),
    "serve".into(),
    "--address".into(),
    ctrl_addr,
    "--credential-env".into(),
    "NEEDY_ACTOR_RUNNER_TOKEN".into(),
    "--roles".into(),
    "actor".into(),
    "--labels".into(),
    "role=actor-e2e".into(),
    "--ephemeral".into(),
    "--gateway".into(),
    gw_addr,
    "--work-dir".into(),
    actor_work.display().to_string(),
    "-v".into(),
  ];
  if let Some(image) = &env.ctx.host.vm_image {
    actor_args.extend(["--actor-image".into(), image.display().to_string()]);
  }
  if let Ok(guest) = &env.ctx.host.guest {
    actor_args.extend(["--guest-needy".into(), guest.display().to_string()]);
  }
  let actor_arg_refs: Vec<&str> = actor_args.iter().map(String::as_str).collect();
  let mut actor_cmd = env.needy_command(&actor_arg_refs);
  actor_cmd
    .env("NEEDY_STATE_DIR", &actor_state)
    .env("NEEDY_RUNTIME_DIR", env.path("ar"))
    .env("NEEDY_CONFIG_DIR", env.path("ac"))
    .env("NEEDY_ACTOR_RUNNER_TOKEN", ACTOR_RUNNER_TOKEN)
    .stdin(Stdio::null())
    .stdout(actor_log.try_clone()?)
    .stderr(actor_log);
  let mut actor_runner = ChildGuard::new(actor_cmd.spawn().context("cannot start ephemeral actor runner")?);

  // Profile: remote Firecracker actor selected via `actor.runner: { labels: [role=actor-e2e] }`,
  // executor workspace mount, routed `python3`, and gateway allowlist (`example.com` allowed,
  // `example.org` denied).
  let routed_check =
    format!("python3 -c 'import os, sys; print(\"E2E routed-marker=\" + str(os.path.exists(\"{}\"))); sys.exit(7)'", marker.display());
  let script = format!(
    r#"echo "E2E ws-seed=$(cat seed.txt 2>/dev/null || echo missing)"
echo from-remote-actor > actor-wrote.txt && echo "E2E ws-write=ok" || echo "E2E ws-write=fail"
{routed_check}
echo "E2E routed-rc=$?"
echo "E2E allow-code=$(curl -sS -o /dev/null --max-time 30 -w '%{{http_code}}' https://example.com/)"
curl -sS -o /dev/null --max-time 30 https://example.org/
echo "E2E deny-rc=$?"
exit 0"#
  );
  let mut actor_block = super::scenarios::actor(&env.ctx.host.image_names, cell, &script);
  actor_block["runner"] = json!({ "labels": ["role=actor-e2e"] });
  actor_block["workspace"] = json!({ "from": format!("executor:{EXECUTOR_NAME}:{}", ws.display()) });
  actor_block["network"] = json!({ "allow": ["example.com"] });
  let (user_name, user_id) = cell.user.account();
  let profile = env.profile(&json!({
    "user": { "name": user_name, "uid": user_id, "gid": user_id },
    "actor": actor_block,
    "executors": {
      EXECUTOR_NAME: { "driver": "native", "runner": EXEC_RUNNER_NAME },
    },
    "commands": [
      { "match": "python3 **", "to": EXECUTOR_NAME },
    ],
  }))?;

  // Create the run with `--wait-for-runner 15s` so the daemon waits for both
  // the executor runner and the ephemeral actor runner to connect.
  let profile_arg = profile.display().to_string();
  let created = env.needy(
    &[
      "run",
      "create",
      "--profile",
      &profile_arg,
      "--wait-for-runner",
      "15s",
    ],
    Duration::from_secs(60),
  )?;
  if !created.ok() {
    bail!("remote-actor run create failed: {}", created.brief());
  }
  let run_id = created.stdout.lines().map(str::trim).rfind(|l| !l.is_empty()).unwrap_or_default().to_string();
  if run_id.is_empty() {
    bail!("expected `<run_id>` from run create, got: {}", created.brief());
  }
  env.runs.push(run_id.clone());

  let initial_run = get_run(&env.runtime(), &run_id)?;

  // Wait for the run to finish and the ephemeral actor runner to exit cleanly.
  let row = runs::wait_end(env, &run_id, env.ctx.timeout)?;
  check_ended(checks, End::Exit, &row, 0);

  let actor_status = actor_runner.wait_exit(Duration::from_secs(20))?;
  checks.check("ephemeral actor runner exits cleanly after VM finishes", actor_status.success(), format!("{actor_status}"));

  // Console output delivered through the outbound bridge stream (`needy run logs`).
  let console = runs::console(env, &run_id)?;
  checks.value(&console, "ws-seed", "from-runner-ws");
  checks.value(&console, "ws-write", "ok");
  checks.value(&console, "routed-marker", "True");
  checks.value(&console, "routed-rc", "7");
  checks.value(&console, "allow-code", "200");
  checks.check(
    "gateway denies host outside allowlist",
    console.value("deny-rc").is_some_and(|rc| rc != "0"),
    format!("{:?}", console.value("deny-rc")),
  );

  // Workspace write from inside the remote actor is visible on the runner.
  checks.eq(
    "workspace write landed on the runner",
    Some("from-remote-actor".to_string()),
    std::fs::read_to_string(ws.join("actor-wrote.txt")).ok().map(|s| s.trim().to_string()),
  );

  // Activity hook fired on Process 2, and `--credential-env` never wrote `runner.json`.
  let hook_content = std::fs::read_to_string(&hook_file).unwrap_or_default();
  checks.check("runner --activity-hook wrote its marker file", hook_content.contains("tick"), format!("content={hook_content:?}"));
  checks.check(
    "runner --credential-env never writes runner.json",
    !runner_state.join("runner.json").exists(),
    runner_state.join("runner.json").display().to_string(),
  );
  checks.check(
    "actor runner --credential-env never writes runner.json",
    !actor_state.join("runner.json").exists(),
    actor_state.join("runner.json").display().to_string(),
  );

  // Report accuracy (`needy run report`).
  let report = runs::report(env, &run_id)?;
  checks.check(
    "report: routed command executed on static runner executor",
    runs::routed_to(&report, EXECUTOR_NAME) >= 1,
    report["commands"].to_string(),
  );
  let network = report["network"].to_string();
  checks.check("report: allowed host recorded in network section", network.contains("example.com"), network);
  let findings = report["findings"].to_string();
  checks.check("report: denied host recorded in findings", findings.contains("network denied: example.org"), findings);
  let kinds = runs::event_kinds(&report);
  for expected in ["actor.start", "exec.decide", "net.deny", "actor.vm.exit"] {
    checks.check(format!("report: trace includes {expected}"), kinds.iter().any(|k| k == expected), format!("{kinds:?}"));
  }

  // `last_activity_ms` advanced past run creation.
  let final_run = get_run(&env.runtime(), &run_id)?;
  checks.check(
    "last_activity_ms advanced during remote actor execution",
    final_run.last_activity_ms > initial_run.created_at_ms && final_run.last_activity_ms >= initial_run.last_activity_ms,
    format!(
      "created_at_ms={}, initial_activity_ms={}, final_activity_ms={}",
      initial_run.created_at_ms, initial_run.last_activity_ms, final_run.last_activity_ms
    ),
  );

  Ok(())
}

/// Fetches a `Run` from the cell's local control socket so the test can inspect
/// wire fields (`created_at_ms`, `last_activity_ms`) directly.
fn get_run(runtime_dir: &Path, run_id: &str) -> Result<needy_api_client::proto::v1::Run> {
  let addr = format!("unix://{}", runtime_dir.join("daemon.sock").display());
  let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
  rt.block_on(async {
    let client = Client::connect(&addr).await.with_context(|| format!("cannot connect to {addr}"))?;
    let resp = client
      .runs()
      .get_run(GetRunRequest {
        run_id: run_id.to_string(),
      })
      .await
      .context("GetRun failed")?
      .into_inner();
    resp.run.context("GetRun returned no run")
  })
}
