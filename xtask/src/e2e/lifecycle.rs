//! Real actor continuity through the public CLI, terminal and OS/runtime
//! boundaries. No stand-in driver, in-process control or process replay.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail, ensure};
use nix::sys::signal::Signal;
use serde_json::{Value, json};

use super::checks::Checks;
use super::env::CellEnv;
use super::matrix::{Cell, Lifecycle};
use super::term::Term;
use super::{proc, runs, scenarios};

const STEP: Duration = Duration::from_secs(30);
const BOOT: Duration = Duration::from_secs(180);
// Source: https://www.rfc-editor.org/rfc/rfc20.html#section-2 (ASCII, 1969).
// Reference: https://github.com/needy-dev/needy/blob/9a2c08e6b5eb0ba921642e7a6e26538b24d96b90/crates/core-executor/src/relay/keys.rs#L127-L149
// Derived with the relay's control-key mask: ']' (0x5d) & 0x1f = GS (0x1d).
const DETACH: &str = "\x1d";

/// The shell variable proves retained process memory: only the original
/// viewer sets it. The file and sequence expose replacement/reexecution.
const PROGRAM: &str = r#"
printf 'start\n' >> "$HOME/.lifecycle-starts"
trap ':' INT
state=unset
sequence=0
printf 'E2E lifecycle-boot=%s\n' "$$"
printf 'LIFECYCLE> '
while IFS= read -r line; do
  case "$line" in
    remember) state=retained-in-memory ;;
    probe)
      sequence=$((sequence+1))
      starts=$(wc -l < "$HOME/.lifecycle-starts")
      printf 'E2E lifecycle-seq=%s pid=%s state=%s starts=%s\n' "$sequence" "$$" "$state" "$starts"
      ;;
    interrupt)
      printf 'E2E INTERRUPT-READY\n'
      sleep 300
      printf 'E2E interrupt-result=%s\n' "$?"
      ;;
    native)
      python3 -c 'import json,os,pathlib,time; p=pathlib.Path("native-starts"); f=p.open("a"); f.write("start\n"); f.close(); tick=0; print("E2E NATIVE-STARTED",flush=True)
while True:
 tick+=1
 pathlib.Path("native-state.json").write_text(json.dumps(dict(pid=os.getpid(),tick=tick)))
 time.sleep(0.1)' "$E2E_LIFECYCLE_MARKER"
      printf 'E2E native-return=%s\n' "$?"
      ;;
  esac
  printf 'LIFECYCLE> '
done
"#;

pub fn run(env: &mut CellEnv, cell: &Cell, journey: Lifecycle, checks: &mut Checks) -> Result<()> {
  let workspace = env.mkdir("lifecycle-native", 0o700)?;
  let (name, uid) = cell.user.account();
  let mut actor = scenarios::actor(&env.ctx.host.image_names, cell, PROGRAM);
  // Also mark the host process argv, so existing leftover detection can
  // report and reclaim this owned child if the product fails its stop.
  actor["env"] = json!({ "E2E_LIFECYCLE_MARKER": workspace });
  let profile = env.profile(&json!({
    "user": { "name": name, "uid": uid, "gid": uid },
    "actor": actor,
    "executors": { "work": { "driver": "native", "workspace": workspace, "path_map": { "~": workspace } } },
    "commands": [{ "match": "python3 **", "to": "work" }],
  }))?;
  let id = runs::create(env, &profile, &["-t"])?;
  runs::wait_actor(env, &id, &["ready"], BOOT)?;
  let mut term = env.term(&["run", "attach", &id])?;
  term.expect("LIFECYCLE> ", STEP)?;
  // Console logs retain complete lines; the prompt above is intentionally
  // unterminated. Read the PID from the newline-terminated startup marker.
  let console = runs::wait_marker(env, &id, "E2E lifecycle-boot=")?;
  let pid = console.value("lifecycle-boot").context("the driving program printed no PID")?.parse::<u32>()?;
  let original = resource(env, &id)?;
  term.send("remember\r")?;
  probe(&mut term, pid, 1)?;
  checks.check("the driving program retains a shell variable and a single start marker", true, "sequence 1, retained-in-memory, starts 1");
  match journey {
    Lifecycle::Continuity => continuity(env, &id, pid, &original, &workspace, term, checks),
    Lifecycle::ActorLoss => actor_loss(env, &id, &original, term, checks),
  }
}

fn probe(term: &mut Term, pid: u32, sequence: u32) -> Result<()> {
  let from = term.mark();
  term.send("probe\r")?;
  term.expect_from(from, &format!("E2E lifecycle-seq={sequence} pid={pid} state=retained-in-memory starts=1"), STEP)?;
  Ok(())
}

fn attach(env: &CellEnv, id: &str, pid: u32, sequence: u32) -> Result<Term> {
  let mut term = env.term(&["run", "attach", id])?;
  term.expect("LIFECYCLE> ", STEP)?;
  probe(&mut term, pid, sequence)?;
  Ok(term)
}

fn detach(term: &mut Term) -> Result<()> {
  term.send(DETACH)?;
  ensure!(term.wait(STEP)? == 0, "viewer detach returned a nonzero exit");
  Ok(())
}

fn continuity(
  env: &mut CellEnv,
  id: &str,
  pid: u32,
  original: &Resource,
  workspace: &Path,
  mut term: Term,
  checks: &mut Checks,
) -> Result<()> {
  detach(&mut term)?;
  let mut term = attach(env, id, pid, 2)?;
  checks.check("detach/attach preserves the original driving process and memory", true, "same PID, sequence 2, one start");
  term.kill()?;
  term.wait(STEP)?;
  let mut term = attach(env, id, pid, 3)?;
  checks.check("terminating the viewer preserves the original driving process and memory", true, "same PID, sequence 3, one start");

  let from = term.mark();
  term.send("interrupt\r")?;
  term.expect_from(from, "E2E INTERRUPT-READY", STEP)?;
  // Source: RFC 20 section 2 (permalink above); ASCII 'C' is 0x43.
  // Derived with the same control-key mask: 0x43 & 0x1f = ETX (0x03).
  term.send("\x03")?;
  // Source: Bash 5.2 reference manual, section 3.7.5 (128 + signal number).
  // Reference: crates/core-executor/src/signals.rs, `shell_code`.
  let interrupted = 128 + Signal::SIGINT as i32;
  term.expect_from(from, &format!("E2E interrupt-result={interrupted}"), STEP)?;
  probe(&mut term, pid, 4)?;
  checks.check(
    "foreground Ctrl-C interrupts the child without deleting the invocation",
    true,
    "same driving PID and shell memory after SIGINT",
  );

  // Dispatch once as an actor foreground command. A lost control stream
  // cannot establish its exit outcome and must not terminate or retry it.
  let from = term.mark();
  term.send("native\r")?;
  term.expect_from(from, "E2E NATIVE-STARTED", STEP)?;
  let mut native = native_state(workspace, None)?;
  let native_pid = i32::try_from(native["pid"].as_i64().context("the native command returned no PID")?)?;
  let native_identity = process_identity(native_pid)?;
  ensure!(native_identity.contains(&workspace.display().to_string()), "the native process does not carry this cell's ownership marker");
  for (index, signal) in [Signal::SIGTERM, Signal::SIGKILL].into_iter().enumerate() {
    let before = needy_daemon::discovery::read(&env.runtime())?;
    env.restart_control(signal)?;
    super::invariants::discovery(env, checks);
    let after = needy_daemon::discovery::read(&env.runtime())?;
    checks.check(
      format!("{signal}: a new actual control instance answers"),
      before.instance_id != after.instance_id,
      "control health instance changed",
    );
    checks.eq(format!("{signal}: the original VM/container resource survives"), original.clone(), resource(env, id)?);
    checks.eq(format!("{signal}: the invocation remains running"), "running", runs::state(&runs::row(env, id)?));
    native = native_state(workspace, native["tick"].as_u64())?;
    checks.eq(
      format!("{signal}: the dispatched native process keeps its original PID"),
      Some(i64::from(native_pid)),
      native["pid"].as_i64(),
    );
    checks.eq(format!("{signal}: the native process has the same OS identity"), native_identity.clone(), process_identity(native_pid)?);
    checks.eq(
      format!("{signal}: the native command was started once"),
      "start\n".to_string(),
      std::fs::read_to_string(workspace.join("native-starts"))?,
    );
    // The old viewer's connection can fail; adoption opens a new viewer of
    // the same terminal. Fresh input must advance the existing shell state.
    drop(term);
    if index == 0 {
      let console = runs::wait_marker(env, id, "E2E native-return=")?;
      checks.value(&console, "native-return", "255");
    }
    term = attach(env, id, pid, 5 + index as u32)?;
    checks.check(
      format!("{signal}: attach resumes the original driving program"),
      true,
      "same PID, retained shell variable and increasing sequence",
    );
    assert_one_actor_start(env, id, checks)?;
  }
  detach(&mut term)?;
  let mut term = attach(env, id, pid, 7)?;
  detach(&mut term)?;
  checks.check("repeated attach/detach still works after both control restarts", true, "sequence 7, original driving program");
  let report = runs::report(env, id)?;
  let dispatches = report["events"]
    .as_array()
    .context("the detailed report has no events")?
    .iter()
    .filter(|event| event["kind"] == "exec.start" && event["payload"]["argv"].to_string().contains("native-starts"))
    .count();
  checks.eq("the trace records one native dispatch across both control gaps", 1, dispatches);
  stop(env, id, original, checks)?;
  checks.check("explicit stop ends the original native process", wait_gone(native_pid), format!("native PID {native_pid}"));
  checks.eq("explicit stop preserves one native effect", "start\n".to_string(), std::fs::read_to_string(workspace.join("native-starts"))?);
  Ok(())
}

fn actor_loss(env: &mut CellEnv, id: &str, original: &Resource, mut term: Term, checks: &mut Checks) -> Result<()> {
  detach(&mut term)?;
  original.kill()?;
  let ended = runs::wait_end(env, id, env.ctx.timeout)?;
  checks.eq("actual actor runtime loss fails the original invocation", "failed", runs::state(&ended));
  checks.check(
    "actual runtime loss records a failure reason",
    ended["failure"].as_str().is_some_and(|reason| !reason.is_empty()),
    ended["failure"].to_string(),
  );
  let report = runs::report(env, id)?;
  checks.eq("the report retains the failed historical outcome", Some("failed"), report["state"].as_str());
  checks.check(
    "the report records actual VM/container exit",
    runs::event_kinds(&report).iter().any(|kind| kind == "actor.vm.exit"),
    "actor.vm.exit",
  );
  assert_one_actor_start(env, id, checks)?;
  let out = env.needy(&["run", "stop", id], env.ctx.timeout)?;
  ensure!(out.ok(), "failed invocation cleanup failed: {}", out.brief());
  checks.eq("cleanup preserves failed history rather than inventing success", "failed", runs::state(&runs::row(env, id)?));
  checks.check("the actual lost resource stays absent", original.wait_gone()?, format!("{original:?}"));
  Ok(())
}

fn stop(env: &CellEnv, id: &str, resource: &Resource, checks: &mut Checks) -> Result<()> {
  let out = env.needy(&["run", "stop", id, "--now"], env.ctx.timeout)?;
  ensure!(out.ok(), "explicit invocation stop failed: {}", out.brief());
  checks.eq("explicit stop records the original invocation as stopped", "stopped", runs::state(&runs::wait_end(env, id, env.ctx.timeout)?));
  checks.check("explicit stop removes the original VM/container resource", resource.wait_gone()?, format!("{resource:?}"));
  assert_one_actor_start(env, id, checks)
}

fn assert_one_actor_start(env: &CellEnv, id: &str, checks: &mut Checks) -> Result<()> {
  let report = runs::report(env, id)?;
  let starts =
    report["events"].as_array().context("the detailed report has no events")?.iter().filter(|event| event["kind"] == "actor.start").count();
  checks.eq("recovery never starts a replacement actor", 1, starts);
  Ok(())
}

fn native_state(workspace: &Path, after_tick: Option<u64>) -> Result<Value> {
  let deadline = Instant::now() + STEP;
  loop {
    if let Ok(bytes) = std::fs::read(workspace.join("native-state.json"))
      && let Ok(state) = serde_json::from_slice::<Value>(&bytes)
      && state["pid"].as_i64().is_some()
      && state["tick"].as_u64().is_some_and(|tick| after_tick.is_none_or(|before| tick > before))
    {
      return Ok(state);
    }
    ensure!(Instant::now() < deadline, "the original native process did not advance its heartbeat");
    std::thread::sleep(Duration::from_millis(50));
  }
}

/// PID plus the OS start time and command line establish continuity; a PID
/// alone could name a replacement process. The harness never adopts it.
fn process_identity(pid: i32) -> Result<String> {
  let out = proc::simple("ps", &["-p", &pid.to_string(), "-ww", "-o", "pid=,lstart=,command="], STEP)?;
  ensure!(out.ok() && !out.stdout.trim().is_empty(), "process {pid} is not observable: {}", out.brief());
  Ok(out.stdout.trim().to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Resource {
  Vm {
    pid: i32,
    process: String,
  },
  Container {
    id: String,
    pid: i64,
    started: String,
  },
  AppleContainer {
    name: String,
    process: String,
  },
}

fn resource(env: &CellEnv, id: &str) -> Result<Resource> {
  let report = runs::report(env, id)?;
  let start = report["events"]
    .as_array()
    .context("the detailed report has no events")?
    .iter()
    .find(|event| event["kind"] == "actor.start")
    .context("the real actor recorded no start")?;
  let pid = start["payload"]["pid"].as_i64().context("actor.start recorded no runtime PID")?;
  let driver = start["payload"]["driver"].as_str().context("actor.start recorded no driver")?;
  match driver {
    "linux-firecracker" | "macos-vz" => {
      let pid = i32::try_from(pid)?;
      let process = process_identity(pid)?;
      let marker = env.state().join("runs").join(id);
      ensure!(process.contains(&marker.display().to_string()), "the VM process does not name this invocation's resource directory");
      Ok(Resource::Vm { pid, process })
    }
    driver if driver.starts_with("linux-container") => {
      let name = format!("needy-actor-{id}");
      let out = proc::simple("docker", &["inspect", "--format", "{{json .}}", &name], STEP)?;
      ensure!(out.ok(), "cannot inspect this actor's real container: {}", out.brief());
      // Source: https://github.com/moby/moby/blob/v28.5.1/api/swagger.yaml
      // Reference: Docker Engine API, ContainerInspect.
      let container: Value = serde_json::from_str(&out.stdout)?;
      ensure!(
        container["Config"]["Labels"]["needy.run"] == id && container["Config"]["Labels"]["needy.role"] == "actor",
        "container labels do not identify the invocation's actor"
      );
      ensure!(container["State"]["Running"] == true, "the actor's container is not running");
      ensure!(container["State"]["Pid"].as_i64() == Some(pid), "the container PID differs from actor.start");
      Ok(Resource::Container {
        id: container["Id"].as_str().context("the runtime returned no container ID")?.into(),
        pid,
        started: container["State"]["StartedAt"].as_str().context("the runtime returned no start time")?.into(),
      })
    }
    "macos-apple-container" => {
      let name = format!("needy-actor-{id}");
      let process = process_identity(i32::try_from(pid)?)?;
      ensure!(process.contains(&name), "the container run process does not name this invocation");
      Ok(Resource::AppleContainer { name, process })
    }
    _ => bail!("no real resource loss probe exists for actor driver {driver}"),
  }
}

/// Shared with file scenarios: abrupt loss means the actor resource itself
/// ends. Killing management is a separate, recoverable lifetime boundary.
pub(super) fn kill_actor(env: &CellEnv, id: &str) -> Result<()> {
  resource(env, id)?.kill()
}

impl Resource {
  fn kill(&self) -> Result<()> {
    match self {
      Self::Vm { pid, process } => {
        ensure!(process_identity(*pid)? == *process, "the VM resource changed before the loss probe");
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(*pid), Signal::SIGKILL).context("cannot kill the owned actor VM")?;
      }
      Self::Container { id, .. } => {
        let out = proc::simple("docker", &["kill", id], STEP)?;
        ensure!(out.ok(), "cannot kill the owned actor container: {}", out.brief());
      }
      Self::AppleContainer { name, .. } => {
        // Delete the VM resource, not merely its `container run` CLI. This is
        // the same per-resource force-delete boundary the driver cleanup uses.
        let out = proc::simple("container", &["delete", "--force", name], STEP)?;
        ensure!(out.ok(), "cannot remove the owned actor VM: {}", out.brief());
      }
    }
    Ok(())
  }

  fn wait_gone(&self) -> Result<bool> {
    let deadline = Instant::now() + STEP;
    loop {
      let gone = match self {
        Self::Vm { pid, .. } => !proc::alive(*pid),
        Self::Container { id, .. } => {
          let filter = format!("id={id}");
          let out = proc::simple("docker", &["ps", "--all", "--quiet", "--no-trunc", "--filter", &filter], STEP)?;
          ensure!(out.ok(), "the runtime could not answer the resource cleanup probe: {}", out.brief());
          out.stdout.trim().is_empty()
        }
        Self::AppleContainer { name, .. } => {
          let out = proc::simple("container", &["ls", "-a", "-q"], STEP)?;
          ensure!(out.ok(), "the runtime could not answer the resource cleanup probe: {}", out.brief());
          !out.stdout.split_whitespace().any(|listed| listed == name)
        }
      };
      if gone || Instant::now() >= deadline {
        return Ok(gone);
      }
      std::thread::sleep(Duration::from_millis(100));
    }
  }
}

fn wait_gone(pid: i32) -> bool {
  let deadline = Instant::now() + STEP;
  while proc::alive(pid) && Instant::now() < deadline {
    std::thread::sleep(Duration::from_millis(50));
  }
  !proc::alive(pid)
}
