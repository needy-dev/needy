//! Each scenario: the profile it writes, the driving program it runs, how
//! the harness drives the run, and the invariants it asserts.
//!
//! Driving programs are POSIX `sh` scripts in the actor. They print their
//! results as `E2E KEY=VALUE` lines and `E2E stat PATH OWNER MODE` lines
//! (see [`super::checks::Console`]); `E2E-READY` tells the harness that a
//! program which waits to be stopped got there. Code routed to an executor
//! is `python3 -c '...'`, since `python3` is on every executor (and the
//! actor's stub for a routed name stands in for it there).

use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use super::checks::{Checks, Console, Stat};
use super::env::CellEnv;
use super::host::ACTOR_IMAGE;
use super::matrix::{Cell, Driver, End, Executor, Fault, Net, RunnerAt, Scenario, Terminal, Workspace};
use super::runs::{self, check_ended, end_run};
use super::transport::{Link, PROXY_IDLE};

/// Helpers every driving program starts with.
const PRELUDE: &str = r#"
e2e_stat() { for p in "$@"; do if [ -e "$p" ]; then stat -c "E2E stat $p %U:%G %a" "$p"; else echo "E2E stat $p absent"; fi; done; }
e2e_wait() { echo E2E-READY; while :; do sleep 1; done; }
"#;

/// The executor every scenario routes to, by its profile name.
const X: &str = "x";

pub fn run(env: &mut CellEnv, cell: &Cell, checks: &mut Checks) -> Result<()> {
  match cell.scenario {
    Scenario::ActorFiles => actor_files(env, cell, checks),
    Scenario::ExecutorFiles => executor_files(env, cell, checks),
    Scenario::Workspace(from) => workspace(env, cell, from, checks),
    Scenario::Routing => routing(env, cell, checks),
    Scenario::Chaos(fault) => chaos(env, cell, fault, checks),
    Scenario::Network(net) => network(env, cell, net, checks),
    Scenario::Identity => identity(env, cell, checks),
    Scenario::Terminal => terminal(env, cell, checks),
    Scenario::FakeAgent => fake_agent(env, cell, checks),
    Scenario::Runner(at) => runner(env, cell, at, checks),
    Scenario::Tui(i) => tui(env, cell, i, checks),
    Scenario::HostExec => host_exec(env, checks),
    Scenario::HostCopy => host_copy(env, checks),
  }
}

// ---------------------------------------------------------------------------
// Profiles

fn user(cell: &Cell) -> Value {
  let (name, id) = cell.user.account();
  json!({ "name": name, "uid": id, "gid": id })
}

/// The actor's home and its owner as `stat -c %U:%G` prints it.
fn home(cell: &Cell) -> (String, String) {
  let (name, _) = cell.user.account();
  (format!("/home/{name}"), format!("{name}:{name}"))
}

/// The profile's `actor`, running `script` under `sh`.
fn actor(cell: &Cell, script: &str) -> Value {
  let command = json!(["/bin/sh", "-c", format!("{PRELUDE}\n{script}")]);
  match cell.driver {
    Driver::Firecracker | Driver::Vz => json!({ "driver": cell.driver.profile_name(), "command": command }),
    Driver::Container(oci) => json!({
      "driver": "linux-container",
      "container": { "oci_runtime": oci.name() },
      "image": ACTOR_IMAGE,
      "command": command,
    }),
    Driver::AppleContainer => json!({ "driver": "macos-apple-container", "image": ACTOR_IMAGE, "command": command }),
    Driver::Host => json!({}),
  }
}

/// The executor `x` and where its files live on it: an absolute host
/// directory for `native`, a path under the executor's home for a
/// container. The actor's home maps to the executor's directory, so a
/// routed command started in the home has a working directory there.
fn executor(env: &CellEnv, kind: Executor) -> Result<(Value, String)> {
  Ok(match kind {
    Executor::Native => {
      let base = env.mkdir("xw", 0o755)?;
      let home = env.mkdir("xh", 0o755)?;
      (json!({ "driver": "native", "path_map": { "~": home.display().to_string() } }), base.display().to_string())
    }
    Executor::Container => (json!({ "driver": "linux-container", "key": "{run}" }), "~/xw".to_string()),
  })
}

/// `python3 -c 'CODE'` for a script; the code must not hold a single quote.
fn py(code: &str) -> String {
  assert!(!code.contains('\''), "routed python code must not contain single quotes");
  format!("python3 -c '{code}'")
}

/// How the program ends: `exit CODE` or waiting to be stopped.
fn ending(end: End, code: i32) -> String {
  match end {
    End::Exit => format!("exit {code}"),
    _ => "e2e_wait".to_string(),
  }
}

/// The copy-in fixture: files and directories with distinct modes, and two
/// entries the nested mapping's filter excludes (`secret.txt`, `cache/`).
fn seed(env: &CellEnv) -> Result<PathBuf> {
  env.file("fx/seed/top.txt", "top\n", 0o644)?;
  env.file("fx/seed/run.sh", "#!/bin/sh\necho run\n", 0o755)?;
  env.file("fx/seed/private.key", "key\n", 0o600)?;
  env.file("fx/seed/sub/deep/leaf.txt", "leaf\n", 0o640)?;
  env.file("fx/seed/secret.txt", "secret\n", 0o644)?;
  env.file("fx/seed/cache/blob.bin", "blob\n", 0o644)?;
  env.mkdir("fx/seed/sub", 0o750)?;
  Ok(env.path("fx/seed"))
}

/// The seed's files with the modes the copy must keep.
const SEED_MODES: &[(&str, &str)] = &[
  ("top.txt", "644"),
  ("run.sh", "755"),
  ("private.key", "600"),
  ("sub", "750"),
  ("sub/deep/leaf.txt", "640"),
];

const OUT_FILTER: &[&str] = &["- *.log", "- /tmp/**", "+ **"];
const SEED_FILTER: &[&str] = &["- /secret.txt", "- /cache/**", "+ **"];

// ---------------------------------------------------------------------------
// Files

fn actor_files(env: &mut CellEnv, cell: &Cell, checks: &mut Checks) -> Result<()> {
  let seed = seed(env)?;
  let (home, owner) = home(cell);
  // Mirror-out creates its destination, not the directory above it.
  let out = env.mkdir("out", 0o755)?;
  let nested = format!("{home}/.config/e2e/nested/seed");
  let single = format!("{home}/single");
  let script = format!(
    r#"cd "$HOME"
N={nested}; S={single}
e2e_stat $HOME/.config $HOME/.config/e2e $HOME/.config/e2e/nested $N {nested_files} $N/secret.txt $N/cache $S {single_files} $S/secret.txt
echo "E2E nested-top=$(cat $N/top.txt)"
echo "E2E single-leaf=$(cat $S/sub/deep/leaf.txt)"
if touch $HOME/.config/e2e/written 2>/dev/null; then echo "E2E nested-parent-writable=yes"; else echo "E2E nested-parent-writable=no"; fi
if mkdir $HOME/.config/other 2>/dev/null; then echo "E2E config-writable=yes"; else echo "E2E config-writable=no"; fi
mkdir -p $HOME/out/sub/deep $HOME/out/tmp
echo keep > $HOME/out/keep.txt; echo deep > $HOME/out/sub/deep/deep.txt; echo log > $HOME/out/skip.log; echo t > $HOME/out/tmp/t.txt
echo "E2E out-written=yes"
{end}"#,
    nested_files = SEED_MODES.iter().map(|(f, _)| format!("$N/{f}")).collect::<Vec<_>>().join(" "),
    single_files = SEED_MODES.iter().map(|(f, _)| format!("$S/{f}")).collect::<Vec<_>>().join(" "),
    end = ending(cell.end, 3),
  );
  let seed_spec = format!("host:{}/", seed.display());
  let profile = json!({
    "user": user(cell),
    "actor": actor(cell, &script),
    "files": {
      "in-nested": { "mode": "copy-in", "from": seed_spec, "to": "actor:~/.config/e2e/nested/seed/", "filter": SEED_FILTER },
      "in-single": { "mode": "copy-in", "from": seed_spec, "to": "actor:~/single/" },
      "out-all": { "mode": "mirror-out", "from": "actor:~/out/", "to": format!("host:{}/all/", out.display()) },
      "out-filtered": { "mode": "mirror-out", "from": "actor:~/out/", "to": format!("host:{}/filtered/", out.display()), "filter": OUT_FILTER },
    },
    "run_files": ["in-nested", "in-single", "out-all", "out-filtered"],
  });
  let profile = env.profile(&profile)?;
  let id = runs::create(env, &profile, &[])?;
  let (row, console) = end_run(env, &id, cell.end, checks, env.ctx.timeout)?;
  check_ended(checks, cell.end, &row, 3);

  // Copy-in: contents, owners and modes, nested and single-level, and the
  // nested mapping's filter.
  for dir in [
    format!("{home}/.config"),
    format!("{home}/.config/e2e"),
    format!("{home}/.config/e2e/nested"),
    nested.clone(),
    single.clone(),
  ] {
    owned_by(checks, &console, "stat", &dir, &owner);
  }
  for (file, mode) in SEED_MODES {
    for base in [&nested, &single] {
      let path = format!("{base}/{file}");
      owned_by(checks, &console, "stat", &path, &owner);
      mode_is(checks, &console, "stat", &path, mode);
    }
  }
  checks.eq("copy-in nested: the filter excludes secret.txt", Some(Stat::Absent), console.stat("stat", &format!("{nested}/secret.txt")));
  checks.eq("copy-in nested: the filter excludes cache/", Some(Stat::Absent), console.stat("stat", &format!("{nested}/cache")));
  checks.check(
    "copy-in single-level without a filter keeps secret.txt",
    matches!(console.stat("stat", &format!("{single}/secret.txt")), Some(Stat::Present { .. })),
    format!("{:?}", console.stat("stat", &format!("{single}/secret.txt"))),
  );
  checks.value(&console, "nested-top", "top");
  checks.value(&console, "single-leaf", "leaf");
  checks.value(&console, "nested-parent-writable", "yes");
  checks.value(&console, "config-writable", "yes");
  checks.value(&console, "out-written", "yes");

  // Mirror-out from the actor, both mappings, for this way of ending.
  if cell.end == End::StopNow {
    // NOTICE: `run stop --now` does not wait for the driving program, so
    // the actor's own mirror-out (init's, after the program exits) is not
    // expected; see the design note's decisions.
    return Ok(());
  }
  if cell.end == End::DaemonKill {
    defer_crash_mirror_out(checks);
    return Ok(());
  }
  mirrored(checks, &out.join("all"), "actor mirror-out", false);
  mirrored(checks, &out.join("filtered"), "actor mirror-out filtered", true);
  Ok(())
}

/// The mirror-outs of a run whose control plane was killed are deferred:
/// the restarted control plane fails the run (`check_ended` asserts that)
/// and mirrors nothing out, since the actor and the executors were lost
/// with it.
// TODO(graceful-shutdown): mirror-out after a control-plane crash is the
// deferred item D1, keeping runs alive across a control-plane restart
// (`TODO(graceful-shutdown)` in `Daemon::shutdown`). When runs survive a
// restart, these cells assert `mirrored` again, as every other end does.
fn defer_crash_mirror_out(checks: &mut Checks) {
  checks.defer("deferred: D1 (runs do not survive a control-plane crash)");
}

/// Checks the owner `USER:GROUP` of `path`; a group of `*` is any group.
fn owned_by(checks: &mut Checks, console: &Console, tag: &str, path: &str, owner: &str) {
  let seen = console.stat(tag, path);
  let matches = |o: &str| match owner.strip_suffix(":*") {
    Some(user) => o.split(':').next() == Some(user),
    None => o == owner,
  };
  checks.check(
    format!("owner of {path}"),
    matches!(&seen, Some(Stat::Present { owner: o, .. }) if matches(o)),
    format!("expected {owner}, got {seen:?}"),
  );
}

fn mode_is(checks: &mut Checks, console: &Console, tag: &str, path: &str, mode: &str) {
  let seen = console.stat(tag, path);
  checks.check(
    format!("mode of {path}"),
    matches!(&seen, Some(Stat::Present { mode: m, .. }) if m == mode),
    format!("expected {mode}, got {seen:?}"),
  );
}

/// Waits briefly for a mirror-out at `dir` and checks what it holds: every
/// file the program wrote, without those the filter excludes when
/// `filtered`.
fn mirrored(checks: &mut Checks, dir: &Path, what: &str, filtered: bool) {
  let deadline = Instant::now() + Duration::from_secs(10);
  while !dir.join("keep.txt").exists() && Instant::now() < deadline {
    std::thread::sleep(Duration::from_millis(200));
  }
  let read = |p: &str| std::fs::read_to_string(dir.join(p)).ok().map(|s| s.trim().to_string());
  checks.eq(format!("{what}: keep.txt on the host"), Some("keep".to_string()), read("keep.txt"));
  checks.eq(format!("{what}: sub/deep/deep.txt on the host"), Some("deep".to_string()), read("sub/deep/deep.txt"));
  if filtered {
    checks.check(format!("{what}: the filter excludes *.log"), !dir.join("skip.log").exists(), "skip.log");
    checks.check(format!("{what}: the filter excludes /tmp/**"), !dir.join("tmp").join("t.txt").exists(), "tmp/t.txt");
  } else {
    checks.eq(format!("{what}: skip.log on the host"), Some("log".to_string()), read("skip.log"));
    checks.eq(format!("{what}: tmp/t.txt on the host"), Some("t".to_string()), read("tmp/t.txt"));
  }
}

/// Python that prints `E2E xstat PATH UID:GID MODE` for each of `paths`
/// under `base` on an executor.
fn xstat(base: &str, paths: &[&str]) -> String {
  let list = paths.iter().map(|p| format!("\"{p}\"")).collect::<Vec<_>>().join(",");
  format!(
    r#"import os
b=os.path.expanduser("{base}")
for p in [{list}]:
    q=os.path.join(b,p)
    if os.path.lexists(q):
        s=os.lstat(q); print("E2E xstat %s %d:%d %o" % (p, s.st_uid, s.st_gid, s.st_mode & 0o7777))
    else:
        print("E2E xstat %s absent" % p)
"#
  )
}

/// The owner a file an executor wrote has there: the harness's user on a
/// native executor, the profile's user in a container.
fn executor_owner(env: &CellEnv, cell: &Cell, kind: Executor) -> String {
  match kind {
    // NOTICE: the group is any: on macOS a new file takes its directory's
    // group (BSD semantics, `open(2)`), so under `/tmp` it is `wheel`.
    Executor::Native => format!("{}:*", env.ctx.host.euid),
    Executor::Container => {
      let (_, id) = cell.user.account();
      format!("{id}:{id}")
    }
  }
}

fn executor_files(env: &mut CellEnv, cell: &Cell, checks: &mut Checks) -> Result<()> {
  let kind = cell.executor.context("executor-files needs an executor")?;
  let seed = seed(env)?;
  // Mirror-out creates its destination, not the directory above it.
  let out = env.mkdir("out", 0o755)?;
  let (block, base) = executor(env, kind)?;
  let mut paths = vec![
    "cfg",
    "cfg/a",
    "cfg/a/b",
    "cfg/a/b/seed",
    "cfg/a/b/seed/secret.txt",
    "cfg/a/b/seed/cache",
    "single",
    "single/secret.txt",
  ];
  let nested: Vec<String> = SEED_MODES.iter().map(|(f, _)| format!("cfg/a/b/seed/{f}")).collect();
  let single: Vec<String> = SEED_MODES.iter().map(|(f, _)| format!("single/{f}")).collect();
  paths.extend(nested.iter().map(String::as_str));
  paths.extend(single.iter().map(String::as_str));
  let write = format!(
    r#"import os
b=os.path.expanduser("{base}")
os.makedirs(os.path.join(b,"out/sub/deep"), exist_ok=True); os.makedirs(os.path.join(b,"out/tmp"), exist_ok=True)
for p, t in [("keep.txt","keep"),("sub/deep/deep.txt","deep"),("skip.log","log"),("tmp/t.txt","t")]:
    open(os.path.join(b,"out",p),"w").write(t + "\n")
print("E2E xout-written=yes")
"#
  );
  let script = format!("cd \"$HOME\"\n{}\necho \"E2E xstat-rc=$?\"\n{}\n{}", py(&xstat(&base, &paths)), py(&write), ending(cell.end, 3));
  let seed_spec = format!("host:{}/", seed.display());
  let profile = json!({
    "user": user(cell),
    "actor": actor(cell, &script),
    "executors": { X: block },
    "commands": [{ "match": "python3 **", "to": X }],
    "files": {
      "x-in-nested": { "mode": "copy-in", "from": seed_spec, "to": format!("executor:{X}:{base}/cfg/a/b/seed/"), "filter": SEED_FILTER },
      "x-in-single": { "mode": "copy-in", "from": seed_spec, "to": format!("executor:{X}:{base}/single/") },
      "x-out-all": { "mode": "mirror-out", "from": format!("executor:{X}:{base}/out/"), "to": format!("host:{}/xall/", out.display()) },
      "x-out-filtered": { "mode": "mirror-out", "from": format!("executor:{X}:{base}/out/"), "to": format!("host:{}/xfiltered/", out.display()), "filter": OUT_FILTER },
    },
    "run_files": ["x-in-nested", "x-in-single", "x-out-all", "x-out-filtered"],
  });
  let profile = env.profile(&profile)?;
  let id = runs::create(env, &profile, &[])?;
  let (row, console) = end_run(env, &id, cell.end, checks, env.ctx.timeout)?;
  check_ended(checks, cell.end, &row, 3);
  checks.value(&console, "xstat-rc", "0");
  checks.value(&console, "xout-written", "yes");
  let owner = executor_owner(env, cell, kind);
  for dir in ["cfg", "cfg/a", "cfg/a/b", "cfg/a/b/seed", "single"] {
    owned_by(checks, &console, "xstat", dir, &owner);
  }
  for (file, mode) in SEED_MODES {
    for base in ["cfg/a/b/seed", "single"] {
      let path = format!("{base}/{file}");
      owned_by(checks, &console, "xstat", &path, &owner);
      // Python's `%o` prints the mode as `stat -c %a` does.
      mode_is(checks, &console, "xstat", &path, mode);
    }
  }
  checks.eq("executor copy-in nested: the filter excludes secret.txt", Some(Stat::Absent), console.stat("xstat", "cfg/a/b/seed/secret.txt"));
  checks.eq("executor copy-in nested: the filter excludes cache/", Some(Stat::Absent), console.stat("xstat", "cfg/a/b/seed/cache"));
  if cell.end == End::DaemonKill {
    defer_crash_mirror_out(checks);
    return Ok(());
  }
  mirrored(checks, &out.join("xall"), "executor mirror-out", false);
  mirrored(checks, &out.join("xfiltered"), "executor mirror-out filtered", true);
  Ok(())
}

// ---------------------------------------------------------------------------
// Workspace

fn workspace(env: &mut CellEnv, cell: &Cell, from: Workspace, checks: &mut Checks) -> Result<()> {
  let (home, owner) = home(cell);
  let ws = format!("{home}/workspace");
  let mut profile = json!({ "user": user(cell) });
  let script;
  match from {
    Workspace::Executor(kind) => {
      let (mut block, base) = executor(env, kind)?;
      // The workspace maps itself; a `~` entry as well would compete with
      // it for the same paths.
      // REVIEW(path-map): with `path_map: { "~": DIR }` and a workspace
      // from the same executor, a routed command in `~/workspace` got
      // `DIR/workspace`, not the workspace's directory (seen 2026-11-13 on
      // macos-apple-container): the explicit entry won over the longer
      // workspace entry. Whether the longest prefix should win is open.
      if let Some(map) = block.as_object_mut() {
        map.remove("path_map");
      }
      // REVIEW(workspace-missing): a container executor's workspace must
      // exist in its image (`~/workspace` does in the executor image). With
      // a path that does not (`~/xw`, seen 2026-11-13 on linux-firecracker),
      // `run create` succeeded, routed commands exited 127 and the actor
      // could not write its workspace; whether needy should create the
      // directory or refuse the run is open.
      let base = if kind == Executor::Container {
        "~/workspace".to_string()
      } else {
        base
      };
      if kind == Executor::Native {
        env.file("xw/pre.txt", "pre\n", 0o640)?;
      }
      let routed = py(
        r#"import os
print("E2E routed-read=" + open("a.txt").read().strip() + "," + open("n/e/s/t/f.txt").read().strip())
open("b.txt","w").write("from-executor\n")
os.makedirs("m/k", exist_ok=True); open("m/k/g.txt","w").write("deep-executor\n")
"#,
      );
      script = format!(
        r#"echo "E2E pwd=$(pwd)"
echo from-actor > a.txt; mkdir -p n/e/s/t; echo nested > n/e/s/t/f.txt
{routed}
echo "E2E routed-rc=$?"
echo "E2E actor-read=$(cat b.txt),$(cat m/k/g.txt)"
e2e_stat {ws}/a.txt {ws}/b.txt {ws}/n {ws}/n/e/s/t/f.txt {ws}/m/k/g.txt
{pre}
exit 0"#,
        pre = if kind == Executor::Native {
          "echo \"E2E pre=$(cat pre.txt)\""
        } else {
          ""
        },
      );
      profile["executors"] = json!({ X: block });
      profile["commands"] = json!([{ "match": "python3 **", "to": X }]);
      profile["actor"] = actor(cell, &script);
      profile["actor"]["workspace"] = json!({ "path": "~/workspace", "from": format!("executor:{X}:{base}"), "writable": true });
    }
    Workspace::Host => {
      env.file("fx/ws/a.txt", "host-a\n", 0o644)?;
      env.file("fx/ws/bin/tool.sh", "#!/bin/sh\n", 0o755)?;
      env.file("fx/ws/deep/x/y.txt", "host-y\n", 0o600)?;
      script = format!(
        r#"echo "E2E pwd=$(pwd)"
echo "E2E host-read=$(cat a.txt),$(cat deep/x/y.txt)"
e2e_stat {ws} {ws}/a.txt {ws}/bin/tool.sh {ws}/deep {ws}/deep/x/y.txt
if echo local > {ws}/new.txt; then echo "E2E writable=yes"; else echo "E2E writable=no"; fi
exit 0"#
      );
      profile["actor"] = actor(cell, &script);
      profile["actor"]["workspace"] =
        json!({ "path": "~/workspace", "from": format!("host:{}", env.path("fx/ws").display()), "writable": true });
    }
  }
  let profile = env.profile(&profile)?;
  let id = runs::create(env, &profile, &[])?;
  let (row, console) = end_run(env, &id, End::Exit, checks, env.ctx.timeout)?;
  check_ended(checks, End::Exit, &row, 0);
  checks.value(&console, "pwd", &ws);
  match from {
    Workspace::Executor(kind) => {
      checks.value(&console, "routed-rc", "0");
      checks.value(&console, "routed-read", "from-actor,nested");
      checks.value(&console, "actor-read", "from-executor,deep-executor");
      for path in ["a.txt", "b.txt", "n", "n/e/s/t/f.txt", "m/k/g.txt"] {
        owned_by(checks, &console, "stat", &format!("{ws}/{path}"), &owner);
      }
      if kind == Executor::Native {
        checks.value(&console, "pre", "pre");
        // The host sees what the actor wrote, owned by the executor's user.
        let host = env.path("xw");
        let read = |p: &str| std::fs::read_to_string(host.join(p)).ok().map(|s| s.trim().to_string());
        checks.eq("native workspace: a.txt on the host", Some("from-actor".into()), read("a.txt"));
        checks.eq("native workspace: n/e/s/t/f.txt on the host", Some("nested".into()), read("n/e/s/t/f.txt"));
        let uid = std::fs::metadata(host.join("a.txt")).map(|m| m.uid()).ok();
        checks.eq("native workspace: a.txt owned by the executor's user on the host", Some(env.ctx.host.euid), uid);
      }
    }
    Workspace::Host => {
      checks.value(&console, "host-read", "host-a,host-y");
      checks.value(&console, "writable", "yes");
      for (path, mode) in [
        ("a.txt", "644"),
        ("bin/tool.sh", "755"),
        ("deep/x/y.txt", "600"),
      ] {
        let path = format!("{ws}/{path}");
        owned_by(checks, &console, "stat", &path, &owner);
        mode_is(checks, &console, "stat", &path, mode);
      }
      owned_by(checks, &console, "stat", &ws, &owner);
      owned_by(checks, &console, "stat", &format!("{ws}/deep"), &owner);
    }
  }
  Ok(())
}

// ---------------------------------------------------------------------------
// Routing

fn routing(env: &mut CellEnv, cell: &Cell, checks: &mut Checks) -> Result<()> {
  let marker = env.file("marker", "here\n", 0o644)?;
  let (block, _) = executor(env, Executor::Native)?;
  let routed_marker = py(&format!("import os; print(\"E2E routed-marker=\" + str(os.path.exists(\"{}\")))", marker.display()));
  let script = format!(
    r#"uname -s > /dev/null; echo "E2E local-rc=$?"
python3 -c "import sys; sys.exit(7)"; echo "E2E routed-rc=$?"
{routed_marker}
if [ -e {marker} ]; then echo "E2E actor-marker=True"; else echo "E2E actor-marker=False"; fi
rm -rf /tmp/e2e-deny-probe; echo "E2E denied-rc=$?"
/bin/sh -c "exit 5"; echo "E2E local-sh-rc=$?"
exit 4"#,
    marker = marker.display(),
  );
  let profile = json!({
    "user": user(cell),
    "actor": actor(cell, &script),
    "executors": { X: block },
    "commands": [
      { "match": "python3 **", "to": X },
      { "match": "rm -rf **", "deny": "e2e: no rm -rf" },
    ],
  });
  let profile = env.profile(&profile)?;
  let id = runs::create(env, &profile, &[])?;
  let (row, console) = end_run(env, &id, End::Exit, checks, env.ctx.timeout)?;
  check_ended(checks, End::Exit, &row, 4);
  checks.value(&console, "local-rc", "0");
  checks.value(&console, "routed-rc", "7");
  // Placement as the program sees it: the marker exists on the host
  // (the native executor) and not in the actor.
  checks.value(&console, "routed-marker", "True");
  checks.value(&console, "actor-marker", "False");
  checks.value(&console, "denied-rc", "126");
  checks.value(&console, "local-sh-rc", "5");
  let report = runs::report(env, &id)?;
  checks.eq("report: python3 routed to x twice", 2, runs::routed_to(&report, X));
  checks.eq("report: one denied command", Some(1), report["commands"]["denied"].as_u64());
  checks.check(
    "report: route names hold python3 and rm",
    report["route_names"].as_array().is_some_and(|r| r.iter().any(|n| n == "python3") && r.iter().any(|n| n == "rm")),
    report["route_names"].to_string(),
  );
  // The trace: every routed exec decided once, with its placement.
  let decides: Vec<&Value> =
    report["events"].as_array().map(|e| e.iter().filter(|e| e["kind"] == "exec.decide").collect()).unwrap_or_default();
  checks.check(
    "trace: exec.decide places python3 on x",
    decides
      .iter()
      .filter(|e| e["payload"].to_string().contains("python3") && e["payload"].to_string().contains(&format!("\"{X}\"")))
      .count()
      >= 2,
    format!("{} exec.decide events", decides.len()),
  );
  checks.check(
    "trace: exec.decide denies rm -rf",
    decides.iter().any(|e| e["payload"].to_string().contains("e2e: no rm -rf")),
    format!("{} exec.decide events", decides.len()),
  );
  checks.check(
    "trace: uname stays in the actor without a decision",
    !decides.iter().any(|e| e["payload"].to_string().contains("uname")),
    "uname is not on the route list",
  );
  Ok(())
}

// ---------------------------------------------------------------------------
// Chaos

fn chaos(env: &mut CellEnv, cell: &Cell, fault: Fault, checks: &mut Checks) -> Result<()> {
  let mut profile = json!({ "user": user(cell) });
  let (block, _) = executor(env, Executor::Native)?;
  profile["executors"] = json!({ X: block });
  let mut service = None;
  let script = match fault {
    Fault::Exec => {
      profile["chaos"] = json!({ "seed": 1, "faults": {
        "exit-seven": { "exec": "e2e-exit **", "exit": 7, "stderr": "e2e-exit-fault\n" },
        "segv": { "exec": "e2e-signal **", "signal": "SIGSEGV" },
      }});
      r#"e2e-exit; echo "E2E exit-rc=$?"
e2e-signal; echo "E2E signal-rc=$?"
exit 0"#
        .to_string()
    }
    Fault::File => {
      env.file("xw/data.json", "{}\n", 0o644)?;
      env.file("xw/ok.txt", "fine\n", 0o644)?;
      profile["chaos"] = json!({ "seed": 1, "faults": { "eio": { "file": { "open": "**/*.json" }, "errno": "EIO" } } });
      r#"cat $HOME/workspace/ok.txt; echo "E2E ok-rc=$?"
echo "E2E json-error=$(cat $HOME/workspace/data.json 2>&1 >/dev/null | head -1)"
cat $HOME/workspace/data.json > /dev/null 2>&1; echo "E2E json-rc=$?"
exit 0"#
        .to_string()
    }
    Fault::Net => {
      let svc = Service::start()?;
      let url = format!("http://127.0.0.1:{}", svc.port);
      service = Some(svc);
      profile["chaos"] = json!({ "seed": 1, "faults": {
        "flaky-503": { "net": { "request": "flaky.needy" }, "status": 503 },
        "refused": { "net": { "connect": "refused.example.com" }, "refuse": true },
        "reset": { "net": { "connect": "reset.example.com" }, "reset": true },
      }});
      profile["e2e_network"] =
        json!({ "allow": ["refused.example.com", "reset.example.com"], "services": { "svc": { "url": url }, "flaky": { "url": url } } });
      r#"echo "E2E flaky-code=$(curl -s -o /dev/null -w '%{http_code}' http://flaky.needy/)"
echo "E2E svc-body=$(curl -s http://svc.needy/ok)"
curl -sS -o /dev/null --max-time 20 https://refused.example.com/; echo "E2E refuse-rc=$?"
curl -sS -o /dev/null --max-time 20 https://reset.example.com/; echo "E2E reset-rc=$?"
exit 0"#
        .to_string()
    }
    Fault::Sensor => {
      profile["chaos"] = json!({ "seed": 1, "faults": { "overheat": { "sensor": { "temp_c": 101 }, "at": "2s" } } });
      r#"echo "E2E sensor-before=$(sensors 2>&1 | grep -m1 -E 'Package|temp1|Core' | tr -s ' ')"
sleep 5
echo "E2E sensor-after=$(sensors 2>&1 | grep -m1 -E 'Package|temp1|Core' | tr -s ' ')"
exit 0"#
        .to_string()
    }
    Fault::Added => {
      profile["commands"] = json!([{ "match": "e2e-later **", "to": X }]);
      r#"echo E2E-READY
i=0; rc=0
while [ $i -lt 240 ]; do e2e-later 2>/dev/null; rc=$?; [ $rc = 9 ] && break; i=$((i+1)); sleep 0.5; done
echo "E2E chaos-add-rc=$rc"
exit 0"#
        .to_string()
    }
  };
  profile["actor"] = actor(cell, &script);
  if fault == Fault::File {
    profile["actor"]["workspace"] =
      json!({ "path": "~/workspace", "from": format!("executor:{X}:{}", env.path("xw").display()), "writable": true });
  }
  if let Some(network) = profile.as_object_mut().and_then(|p| p.remove("e2e_network")) {
    profile["actor"]["network"] = network;
  }
  let profile = env.profile(&profile)?;
  let id = runs::create(env, &profile, &[])?;
  if fault == Fault::Added {
    runs::wait_marker(env, &id, "E2E-READY")?;
    let out = env.needy(
      &[
        "chaos",
        "add",
        "later",
        "--run",
        &id,
        "--exec",
        "e2e-later **",
        "--exit",
        "9",
      ],
      Duration::from_secs(60),
    )?;
    checks.check("`needy chaos add` succeeds", out.ok(), out.brief());
    let listed = env.needy(&["chaos", "ls", "--run", &id], Duration::from_secs(60))?;
    checks.check("`needy chaos ls` lists the added fault", listed.ok() && listed.stdout.contains("later"), listed.brief());
  }
  let (row, console) = end_run(env, &id, End::Exit, checks, env.ctx.timeout)?;
  check_ended(checks, End::Exit, &row, 0);
  let report = runs::report(env, &id)?;
  let injected = runs::event_kinds(&report).iter().filter(|k| *k == "fault.injected").count();
  match fault {
    Fault::Exec => {
      checks.value(&console, "exit-rc", "7");
      // sh reports a child killed by signal N as 128 + N; SIGSEGV is 11.
      checks.value(&console, "signal-rc", "139");
      checks.check("the exec fault's stderr reaches the program", console.contains("e2e-exit-fault"), runs::tail(&console.text));
      checks.check("trace: fault.injected for both exec faults", injected >= 2, format!("{injected} injected"));
    }
    Fault::File => {
      checks.value(&console, "ok-rc", "0");
      checks.value(&console, "json-rc", "1");
      checks.check(
        "the file fault's EIO reaches the program",
        console.value("json-error").is_some_and(|e| e.contains("Input/output error")),
        format!("{:?}", console.value("json-error")),
      );
      checks.check("trace: fault.injected for the file fault", injected >= 1, format!("{injected} injected"));
    }
    Fault::Net => {
      checks.value(&console, "flaky-code", "503");
      checks.value(&console, "svc-body", "e2e-service-ok");
      // NOTICE: through the gateway's CONNECT tunnel, curl reports a
      // refused upstream as a failed tunnel (56) or a refused connection
      // (7), and a reset one as an empty reply (52), a failed receive (56)
      // or a TLS failure (35), depending on where the gateway cuts it.
      checks.check(
        "a refused connection fails curl",
        matches!(console.value("refuse-rc").as_deref(), Some("7" | "56")),
        format!("{:?}", console.value("refuse-rc")),
      );
      checks.check(
        "a reset connection fails curl",
        matches!(console.value("reset-rc").as_deref(), Some("35" | "52" | "56")),
        format!("{:?}", console.value("reset-rc")),
      );
      checks.check("trace: fault.injected for the net faults", injected >= 3, format!("{injected} injected"));
      drop(service);
    }
    Fault::Sensor => {
      checks.check(
        "the overheat shows in sensors",
        console.value("sensor-after").is_some_and(|s| s.contains("+101.0")),
        format!("before {:?}, after {:?}", console.value("sensor-before"), console.value("sensor-after")),
      );
      checks.check(
        "sensors read normal before the fault",
        console.value("sensor-before").is_some_and(|s| !s.is_empty() && !s.contains("+101.0")),
        format!("{:?}", console.value("sensor-before")),
      );
    }
    Fault::Added => {
      checks.value(&console, "chaos-add-rc", "9");
      checks.check("trace: chaos.add recorded", runs::event_kinds(&report).iter().any(|k| k == "chaos.add"), "chaos.add");
      checks.check("trace: the added fault fired", injected >= 1, format!("{injected} injected"));
    }
  }
  Ok(())
}

/// A plain-HTTP server on the control host's loopback, the far end of a
/// service: it answers `e2e-service-ok` and records each request line.
struct Service {
  port: u16,
  requests: Arc<Mutex<Vec<String>>>,
  stop: Arc<AtomicBool>,
  thread: Option<JoinHandle<()>>,
}

impl Service {
  fn start() -> Result<Self> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let port = listener.local_addr()?.port();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let (seen, stopped) = (requests.clone(), stop.clone());
    let thread = std::thread::spawn(move || {
      while !stopped.load(Ordering::Relaxed) {
        match listener.accept() {
          Ok((mut stream, _)) => {
            // One request per connection, small: a bounded read is enough.
            let _ = stream.set_nonblocking(false);
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).unwrap_or(0);
            let head = String::from_utf8_lossy(&buf[..n]);
            if let Some(line) = head.lines().next() {
              seen.lock().map(|mut s| s.push(line.to_string())).ok();
            }
            let body = "e2e-service-ok";
            // A client that went away needs no answer.
            let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
          }
          Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(20)),
          Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
      }
    });
    Ok(Self {
      port,
      requests,
      stop,
      thread: Some(thread),
    })
  }

  fn requests(&self) -> Vec<String> {
    self.requests.lock().map(|r| r.clone()).unwrap_or_default()
  }
}

impl Drop for Service {
  fn drop(&mut self) {
    self.stop.store(true, Ordering::Relaxed);
    if let Some(thread) = self.thread.take() {
      let _ = thread.join();
    }
  }
}

// ---------------------------------------------------------------------------
// Network

fn network(env: &mut CellEnv, cell: &Cell, net: Net, checks: &mut Checks) -> Result<()> {
  let svc = Service::start()?;
  let script = match net {
    Net::Allow => {
      r#"echo "E2E allow-code=$(curl -sS -o /dev/null --max-time 30 -w '%{http_code}' https://example.com/)"
exit 0"#
    }
    Net::Deny => {
      r#"curl -sS -o /dev/null --max-time 30 https://example.org/; echo "E2E deny-rc=$?"
exit 0"#
    }
    Net::Service => {
      r#"echo "E2E svc-body=$(curl -sS --max-time 30 http://svc.needy/hello)"
exit 0"#
    }
  };
  let mut actor = actor(cell, script);
  actor["network"] = json!({ "allow": ["example.com"], "services": { "svc": { "url": format!("http://127.0.0.1:{}", svc.port) } } });
  let profile = env.profile(&json!({ "user": user(cell), "actor": actor }))?;
  let id = runs::create(env, &profile, &[])?;
  let (row, console) = end_run(env, &id, End::Exit, checks, env.ctx.timeout)?;
  check_ended(checks, End::Exit, &row, 0);
  let report = runs::report(env, &id)?;
  match net {
    Net::Allow => {
      checks.value(&console, "allow-code", "200");
      let hosts = report["network"].to_string();
      checks.check("report: the allowed host is in the network section", hosts.contains("example.com"), hosts);
    }
    Net::Deny => {
      checks.check(
        "a host outside the allowlist is refused",
        console.value("deny-rc").is_some_and(|rc| rc != "0"),
        format!("{:?}", console.value("deny-rc")),
      );
      let findings = report["findings"].to_string();
      checks.check("report: a finding names the denied host", findings.contains("network denied: example.org"), findings);
    }
    Net::Service => {
      checks.value(&console, "svc-body", "e2e-service-ok");
      let requests = svc.requests();
      checks.check("the service's server got the request", requests.iter().any(|r| r.starts_with("GET /hello")), format!("{requests:?}"));
    }
  }
  Ok(())
}

// ---------------------------------------------------------------------------
// Identity

fn identity(env: &mut CellEnv, cell: &Cell, checks: &mut Checks) -> Result<()> {
  let kind = cell.executor.context("identity needs an executor")?;
  let (block, _) = executor(env, kind)?;
  let (name, id) = cell.user.account();
  let (home, _) = home(cell);
  let routed = py(
    r#"import os,pwd
e=pwd.getpwuid(os.getuid())
print("E2E x-name=" + e.pw_name); print("E2E x-uid=%d" % os.getuid()); print("E2E x-home=" + os.environ.get("HOME",""))
print("E2E x-pwdir=" + e.pw_dir); print("E2E x-tilde=" + os.path.expanduser("~"))
"#,
  );
  let script = format!(
    r#"echo "E2E a-whoami=$(whoami)"; echo "E2E a-idname=$(id -un)"; echo "E2E a-uid=$(id -u)"; echo "E2E a-gid=$(id -g)"
echo "E2E a-home=$HOME"; echo "E2E a-user=${{USER:-}}"; echo "E2E a-logname=${{LOGNAME:-}}"
echo "E2E a-getent=$(getent passwd $(id -un) | cut -d: -f1,3,4,6)"; echo "E2E a-tilde=$(cd ~ && pwd)"
cd "$HOME"
{routed}
echo "E2E x-rc=$?"
exit 0"#
  );
  let profile = json!({
    "user": user(cell),
    "actor": actor(cell, &script),
    "executors": { X: block },
    "commands": [{ "match": "python3 **", "to": X }],
  });
  let profile = env.profile(&profile)?;
  let run = runs::create(env, &profile, &[])?;
  let (row, console) = end_run(env, &run, End::Exit, checks, env.ctx.timeout)?;
  check_ended(checks, End::Exit, &row, 0);
  for (key, value) in [
    ("a-whoami", name.to_string()),
    ("a-idname", name.to_string()),
    ("a-uid", id.to_string()),
    ("a-gid", id.to_string()),
    ("a-home", home.clone()),
    ("a-user", name.to_string()),
    ("a-logname", name.to_string()),
    ("a-getent", format!("{name}:{id}:{id}:{home}")),
    ("a-tilde", home.clone()),
  ] {
    checks.value(&console, key, &value);
  }
  checks.value(&console, "x-rc", "0");
  match kind {
    Executor::Container => {
      checks.value(&console, "x-name", name);
      checks.value(&console, "x-uid", &id.to_string());
      checks.value(&console, "x-home", &home);
      checks.value(&console, "x-pwdir", &home);
      checks.value(&console, "x-tilde", &home);
    }
    Executor::Native => {
      // A native executor is the runner's own user; its name, home and
      // `~` still agree with each other.
      checks.value(&console, "x-uid", &env.ctx.host.euid.to_string());
      let pwdir = console.value("x-pwdir");
      checks.eq("native executor: HOME agrees with the passwd entry", pwdir.clone(), console.value("x-home"));
      checks.eq("native executor: ~ agrees with the passwd entry", pwdir, console.value("x-tilde"));
    }
  }
  Ok(())
}

// ---------------------------------------------------------------------------
// Terminals

const TERMINAL_PROGRAM: &str = r#"printf 'E2E-PROMPT> '; read line; echo "E2E got=$line"
if [ -t 0 ]; then echo "E2E tty=yes"; else echo "E2E tty=no"; fi
echo "E2E size=$(stty size)"
printf 'E2E-PROMPT2> '; read line2; echo "E2E got2=$line2"
exit 6"#;

const STEP: Duration = Duration::from_secs(60);
/// The detach key `ctrl-]`.
const DETACH: &str = "\x1d";

fn terminal(env: &mut CellEnv, cell: &Cell, checks: &mut Checks) -> Result<()> {
  let profile = env.profile(&json!({ "user": user(cell), "actor": actor(cell, TERMINAL_PROGRAM) }))?;
  let profile = profile.display().to_string();
  match cell.terminal {
    Terminal::Now => {
      let mut term = env.term(&[
        "run",
        "create",
        "-it",
        "--profile",
        &profile,
        "--name",
        "it",
      ])?;
      term.expect("E2E-PROMPT> ", Duration::from_secs(180))?;
      term.send("hello\r")?;
      term.expect("E2E got=hello", STEP)?;
      term.expect("E2E tty=yes", STEP)?;
      term.expect("E2E size=30 100", STEP)?;
      term.expect("E2E-PROMPT2> ", STEP)?;
      term.send("bye\r")?;
      term.expect("E2E got2=bye", STEP)?;
      let code = term.wait(STEP)?;
      checks.eq("run create -it returns the program's exit code", 6, code);
      let row = runs::wait_end(env, "it", STEP)?;
      if let Some(id) = row["id"].as_str() {
        env.runs.push(id.to_string());
      }
      check_ended(checks, End::Exit, &row, 6);
      checks.eq("run ls: tty", Some(true), row["tty"].as_bool());
    }
    Terminal::Later => {
      let id = runs::create(env, Path::new(&profile), &["-t"])?;
      // Ready means `run attach` works (TERMS_AND_CONCEPTS.md, Run).
      let live = runs::wait_actor(env, &id, &["ready"], Duration::from_secs(180))?;
      checks.eq("run ls before attaching: tty", Some(true), live["tty"].as_bool());
      super::invariants::sockets(env, checks, "while the run is live");
      let mut first = env.term(&["run", "attach", &id])?;
      first.expect("E2E-PROMPT> ", STEP)?;
      first.send("first\r")?;
      first.expect("E2E got=first", STEP)?;
      first.expect("E2E tty=yes", STEP)?;
      first.expect("E2E-PROMPT2> ", STEP)?;
      first.send(DETACH)?;
      let code = first.wait(STEP)?;
      checks.eq("run attach returns 0 when detached", 0, code);
      let row = runs::row(env, &id)?;
      checks.eq("run ls after detaching: state", "running", runs::state(&row));
      checks.eq("run ls after detaching: actor", Some("ready"), row["actor"].as_str());
      let mut second = env.term(&["run", "attach", &id])?;
      // The replay redraws the screen the program left.
      second.expect("E2E-PROMPT2> ", STEP)?;
      let mark = second.mark();
      second.send("second\r")?;
      second.expect_from(mark, "E2E got2=second", STEP)?;
      let code = second.wait(STEP)?;
      checks.eq("run attach returns the program's exit code", 6, code);
      let row = runs::wait_end(env, &id, STEP)?;
      check_ended(checks, End::Exit, &row, 6);
    }
    Terminal::None => bail!("the terminal scenario needs -t or -it"),
  }
  Ok(())
}

fn fake_agent(env: &mut CellEnv, cell: &Cell, checks: &mut Checks) -> Result<()> {
  let state = env.mkdir("agent-state", 0o700)?;
  let (block, _) = executor(env, Executor::Native)?;
  let (home, owner) = home(cell);
  let cfg = format!("{home}/.config/fake-agent");
  let script = format!(
    r#"C={cfg}
e2e_stat $HOME/.config $C $C/auth $C/auth/token.json
if [ -f $C/auth/token.json ]; then echo "E2E logged-in=$(cat $C/auth/token.json)"; else echo "E2E logged-in=none"; fi
printf 'fake-agent> '
while read -r line; do
  case "$line" in
    login) mkdir -p $C/auth $C/sessions $C/cache && (umask 077; echo '{{"user":"e2e"}}' > $C/auth/token.json) && echo cached > $C/cache/blob && echo "E2E login=done" || echo "E2E login=failed" ;;
    work) bash -c 'echo "E2E routed-shell=$(uname -s)"'; echo "E2E work-rc=$?"; date >> $C/sessions/s1.jsonl ;;
  esac
  printf 'fake-agent> '
done
e2e_wait"#
  );
  let state_spec = format!("host:{}/", state.display());
  let filter = ["- /cache/**", "+ **"];
  let profile = json!({
    "user": user(cell),
    "actor": actor(cell, &script),
    "executors": { X: block },
    "commands": [{ "match": "bash **", "to": X }],
    "files": {
      "login-in": { "mode": "copy-in", "from": state_spec, "to": "actor:~/.config/fake-agent/", "filter": filter },
      "login-out": { "mode": "mirror-out", "from": "actor:~/.config/fake-agent/", "to": state_spec, "filter": filter },
    },
    "run_files": ["login-in", "login-out"],
  });
  let profile = env.profile(&profile)?;

  // First run: log in, work, detach, stop.
  let first = runs::create(env, &profile, &["-t"])?;
  runs::wait_actor(env, &first, &["ready"], Duration::from_secs(180))?;
  let mut term = env.term(&["run", "attach", &first])?;
  term.expect("fake-agent> ", STEP)?;
  checks.check("first run: not logged in yet", term.transcript().contains("E2E logged-in=none"), "logged-in=none");
  term.send("login\r")?;
  term.expect("E2E login=done", STEP)?;
  term.send("work\r")?;
  term.expect("E2E work-rc=0", STEP)?;
  let transcript = Console::new(term.transcript());
  checks.check(
    "a routed shell ran",
    transcript.value("routed-shell").is_some_and(|s| !s.is_empty()),
    format!("{:?}", transcript.value("routed-shell")),
  );
  term.send(DETACH)?;
  checks.eq("detaching leaves run attach with 0", 0, term.wait(STEP)?);
  super::invariants::sockets(env, checks, "while the run is live");
  let out = env.needy(&["run", "stop", &first], env.ctx.timeout)?;
  checks.check("`needy run stop` succeeds", out.ok(), out.brief());
  let row = runs::wait_end(env, &first, env.ctx.timeout)?;
  check_ended(checks, End::Stop, &row, 0);
  let report = runs::report(env, &first)?;
  checks.check("report: the shell was routed to x", runs::routed_to(&report, X) >= 1, report["commands"].to_string());

  // The login came back to the host, private, without the cache.
  let token = state.join("auth").join("token.json");
  let deadline = Instant::now() + Duration::from_secs(10);
  while !token.exists() && Instant::now() < deadline {
    std::thread::sleep(Duration::from_millis(200));
  }
  checks.eq(
    "mirror-out on stop: the token is on the host",
    Some("{\"user\":\"e2e\"}".to_string()),
    std::fs::read_to_string(&token).ok().map(|s| s.trim().to_string()),
  );
  let mode = std::fs::metadata(&token).map(|m| m.permissions().mode() & 0o777).ok();
  checks.check("mirror-out keeps the token private (no group or other bits)", mode.is_some_and(|m| m & 0o077 == 0), format!("{mode:?}"));
  checks.check("mirror-out on stop: the session log is on the host", state.join("sessions").join("s1.jsonl").exists(), "sessions/s1.jsonl");
  checks.check("mirror-out filter excludes cache/", !state.join("cache").exists(), "cache/");

  // Second run: the login is back, under a home the user owns.
  let second = runs::create(env, &profile, &["-t"])?;
  runs::wait_actor(env, &second, &["ready"], Duration::from_secs(180))?;
  let mut term = env.term(&["run", "attach", &second])?;
  term.expect("fake-agent> ", STEP)?;
  let transcript = Console::new(term.transcript());
  checks.value(&transcript, "logged-in", "{\"user\":\"e2e\"}");
  for path in [
    format!("{home}/.config"),
    cfg.clone(),
    format!("{cfg}/auth"),
    format!("{cfg}/auth/token.json"),
  ] {
    owned_by(checks, &transcript, "stat", &path, &owner);
  }
  term.send(DETACH)?;
  term.wait(STEP)?;
  let out = env.needy(&["run", "stop", &second], env.ctx.timeout)?;
  checks.check("`needy run stop` succeeds (second run)", out.ok(), out.brief());
  runs::wait_end(env, &second, env.ctx.timeout)?;
  Ok(())
}

// ---------------------------------------------------------------------------
// Runners

fn runner(env: &mut CellEnv, cell: &Cell, at: RunnerAt, checks: &mut Checks) -> Result<()> {
  let t = Duration::from_secs(120);
  let out = env.needy(
    &[
      "runner", "token", "create", "--ttl", "10m", "--roles", "executor",
    ],
    t,
  )?;
  if !out.ok() {
    bail!("runner token create failed: {}", out.brief());
  }
  let token = out.stdout.lines().map(str::trim).rfind(|l| !l.is_empty()).unwrap_or_default().to_string();
  // A runner's name is a DNS label; the cell's directory makes it unique
  // where several cells share a Windows runner.
  let suffix = env.dir.file_name().and_then(|n| n.to_str()).unwrap_or("x").trim_start_matches("ne2e-").to_string();
  let name = match at {
    RunnerAt::Local(_) => "e2e-local".to_string(),
    RunnerAt::Windows => format!("e2e-w{suffix}"),
  };
  let mut link = None;
  let windows = match at {
    RunnerAt::Local(transport) => {
      let port = env.tcp.context("the daemon listens on no TCP port")?;
      let started = Link::start(transport, &env.ctx.transports, port, &env.dir.join("transport.log"))?;
      env.start_local_runner(&token, &name, &started.endpoint, &started.env)?;
      link = Some(started);
      None
    }
    RunnerAt::Windows => {
      let windows = env.ctx.windows.clone().context("no Windows runner")?.map_err(anyhow::Error::msg)?;
      let port = env.tcp.context("the daemon listens on no TCP port")?;
      env.add_runner(windows.start(port, &token, &name)?);
      Some(windows)
    }
  };

  // The runner joins and reports itself online, as `runner ls` shows it.
  let deadline = Instant::now() + Duration::from_secs(180);
  let row = loop {
    let out = env.needy(&["runner", "ls", "-o", "json"], t)?;
    let rows: Value = serde_json::from_str(&out.stdout).unwrap_or_default();
    let row = rows.as_array().and_then(|r| r.iter().find(|r| r["name"] == name.as_str())).cloned();
    if let Some(row) = &row
      && row.to_string().contains("online")
    {
      break row.clone();
    }
    if Instant::now() > deadline {
      bail!("runner {name} did not come online within 180 s: {}", out.brief());
    }
    std::thread::sleep(Duration::from_millis(500));
  };
  checks.check("runner ls: the runner is online with executor(native)", row.to_string().contains("native"), row.to_string());

  let exec: Vec<&str> = match at {
    RunnerAt::Local(_) => vec!["exec", "--runner", &name, "--", "sh", "-c", "exit 5"],
    RunnerAt::Windows => vec!["exec", "--runner", &name, "--", "cmd", "/c", "exit 5"],
  };
  let out = env.needy(&exec, t)?;
  checks.check("exec --runner: the command's exit code is 5", out.code == Some(5), out.brief());

  let marker = env.file("marker", "here\n", 0o644)?;
  let (block, script, rule, workspace) = match at {
    RunnerAt::Local(_) => {
      let base = env.mkdir("xw", 0o755)?;
      let routed_marker = py(&format!("import os; print(\"E2E routed-marker=\" + str(os.path.exists(\"{}\")))", marker.display()));
      // The workspace comes from the runner's executor over the transport:
      // it stays idle past the idle proxy's limit, then the harness drops
      // every connection of the transport (at `cut-me`) and writes
      // `cut.done` on the runner's side; the actor waits to see it through
      // the mount, which has to come back.
      // REVIEW(path-map): no `~` entry beside the workspace (see `workspace`).
      (
        json!({ "driver": "native", "runner": name }),
        format!(
          r#"python3 -c "import sys; sys.exit(7)"; echo "E2E routed-rc=$?"
{routed_marker}
echo before > before.txt && echo "E2E before=ok" || echo "E2E before=fail"
sleep {idle}
cat before.txt >/dev/null 2>&1 && echo "E2E after-idle=ok" || echo "E2E after-idle=fail"
echo "E2E cut-me=now"
i=0; while [ $i -lt 120 ]; do [ -f cut.done ] && break; sleep 1; i=$((i+1)); done
cat cut.done >/dev/null 2>&1 && echo after > after.txt && echo "E2E after-cut=ok" || echo "E2E after-cut=fail"
python3 -c "import sys; sys.exit(3)"; echo "E2E routed-after-cut-rc=$?"
exit 0"#,
          idle = (PROXY_IDLE + Duration::from_secs(15)).as_secs()
        ),
        "python3 **",
        Some(base),
      )
    }
    RunnerAt::Windows => (
      // `~` there is the Windows user's home.
      json!({ "driver": "native", "runner": name, "path_map": { "~": "~" } }),
      "cmd /c exit 7; echo \"E2E routed-rc=$?\"\necho \"E2E win-os=$(cmd /c echo %OS%)\"\nexit 0".to_string(),
      "cmd **",
      None,
    ),
  };
  let mut profile = json!({
    "user": user(cell),
    "actor": actor(cell, &script),
    "executors": { X: block },
    "commands": [{ "match": rule, "to": X }],
  });
  if let Some(base) = &workspace {
    profile["actor"]["workspace"] = json!({ "path": "~/workspace", "from": format!("executor:{X}:{}", base.display()), "writable": true });
  }
  let profile = env.profile(&profile)?;
  let id = runs::create(env, &profile, &[])?;
  if let (Some(link), Some(base)) = (&mut link, &workspace) {
    runs::wait_marker(env, &id, "E2E cut-me=now")?;
    if link.transport.can_cut() {
      link.cut()?;
    }
    std::fs::write(base.join("cut.done"), "cut\n")?;
  }
  let (row, console) = end_run(env, &id, End::Exit, checks, env.ctx.timeout)?;
  check_ended(checks, End::Exit, &row, 0);
  checks.value(&console, "routed-rc", "7");
  match at {
    RunnerAt::Local(transport) => {
      checks.value(&console, "routed-marker", "True");
      checks.value(&console, "before", "ok");
      // Idle past the proxy's limit: needy's keepalives hold the runner's
      // connections, so the mount answers without reconnecting.
      checks.value(&console, "after-idle", "ok");
      // Every connection dropped (not for `direct`): the runner reconnects,
      // and the mount and routed commands work again.
      checks.value(&console, "after-cut", "ok");
      checks.value(&console, "routed-after-cut-rc", "3");
      let cut = if transport.can_cut() {
        " after every connection was dropped"
      } else {
        ""
      };
      let after = workspace.as_ref().and_then(|b| std::fs::read_to_string(b.join("after.txt")).ok());
      checks.check(
        format!("the actor wrote after.txt on the runner through the mount{cut}"),
        after.as_deref() == Some("after\n"),
        format!("{after:?}"),
      );
    }
    RunnerAt::Windows => {
      checks.value(&console, "win-os", "Windows_NT");
    }
  }
  let report = runs::report(env, &id)?;
  checks.check("report: commands routed to the runner's executor", runs::routed_to(&report, X) >= 1, report["commands"].to_string());
  let out = env.needy(&["run", "stop", &id], t)?;
  checks.check("run stop releases the runner's executor", out.ok(), out.brief());

  if let Some(windows) = windows {
    // NOTICE: Windows OpenSSH does not end a session's processes when the
    // session closes, so the runner outlives its ssh client (as a runner
    // should). The harness removes it from the control plane, ends the
    // processes it started, then checks that none is left.
    let out = env.needy(&["runner", "rm", &name], t)?;
    checks.check("runner rm removes the runner", out.ok(), out.brief());
    env.stop_runners();
    if let Err(e) = windows.leftovers(&name) {
      checks.check("the Windows runner's processes can be ended", false, format!("{e:#}"));
    }
    std::thread::sleep(Duration::from_secs(2));
    match windows.leftovers(&name) {
      Ok(n) => {
        checks.eq("no needy.exe left on the Windows runner", 0, n);
      }
      Err(e) => {
        checks.check("no needy.exe left on the Windows runner", false, format!("{e:#}"));
      }
    }
    windows.clean(&name);
  }
  Ok(())
}

fn tui(env: &mut CellEnv, cell: &Cell, index: usize, checks: &mut Checks) -> Result<()> {
  let cases = super::tui::cases();
  let case = cases.get(index).context("no such TUI case")?;
  super::tui::run(env, cell, case, checks)
}

// ---------------------------------------------------------------------------
// Without an actor

fn host_exec(env: &mut CellEnv, checks: &mut Checks) -> Result<()> {
  let t = Duration::from_secs(60);
  let out = env.needy(&["exec", "--", "sh", "-c", "exit 7"], t)?;
  checks.eq("exec: the command's exit code", Some(7), out.code);
  let out = env.needy(&["exec", "--", "sh", "-c", "echo out; echo err >&2"], t)?;
  checks.eq("exec: stdout", "out", out.stdout.trim());
  checks.eq("exec: stderr", "err", out.stderr.trim());
  let out = env.needy(&["exec", "--", "e2e-no-such-command"], t)?;
  checks.eq("exec: a missing command exits 127", Some(127), out.code);
  let out = env.needy(
    &[
      "exec",
      "--output",
      "json",
      "--",
      "sh",
      "-c",
      "echo hi; exit 3",
    ],
    t,
  )?;
  checks.eq("exec --output json: the process's exit code", Some(3), out.code);
  let json: Option<Value> = serde_json::from_str(out.stdout.trim()).ok();
  checks.check("exec --output json prints one JSON object", json.as_ref().is_some_and(Value::is_object), out.brief());
  let mut term = env.term(&[
    "exec",
    "-t",
    "--",
    "sh",
    "-c",
    "test -t 0 && test -t 1 && echo E2E-ON-TTY && exit 4",
  ])?;
  term.expect("E2E-ON-TTY", t)?;
  checks.eq("exec -t: a terminal on both ends, and its exit code", 4, term.wait(t)?);
  let out = env.needy(&["exec", "-d", "--", "sleep", "60"], t)?;
  let process = out.stdout.trim().to_string();
  checks.check("exec -d prints a process ID", out.ok() && !process.is_empty(), out.brief());
  let ps = env.needy(&["ps"], t)?;
  checks.check("ps lists the detached process", ps.stdout.contains(&process), ps.brief());
  let kill = env.needy(&["kill", &process], t)?;
  checks.check("kill ends the detached process", kill.ok(), kill.brief());
  Ok(())
}

fn host_copy(env: &mut CellEnv, checks: &mut Checks) -> Result<()> {
  let t = Duration::from_secs(60);
  let seed = seed(env)?;
  let dest = env.path("xw/a/b/c");
  let out = env.needy(
    &[
      "fs",
      "cp",
      &seed.display().to_string(),
      &format!("local:{}", dest.display()),
    ],
    t,
  )?;
  checks.check("fs cp into a nested directory that does not exist", out.ok(), out.brief());
  for (file, mode) in SEED_MODES {
    let path = dest.join("seed").join(file);
    let meta = std::fs::metadata(&path).ok();
    checks.eq(
      format!("fs cp: mode of seed/{file}"),
      Some(mode.to_string()),
      meta.as_ref().map(|m| format!("{:o}", m.permissions().mode() & 0o7777)),
    );
    checks.eq(format!("fs cp: owner of seed/{file}"), Some(env.ctx.host.euid), meta.map(|m| m.uid()));
  }
  let back = env.path("out/back");
  std::fs::create_dir_all(&back)?;
  let out = env.needy(
    &[
      "fs",
      "cp",
      &format!("local:{}", dest.join("seed").display()),
      &back.display().to_string(),
    ],
    t,
  )?;
  checks.check("fs cp back from the executor", out.ok(), out.brief());
  checks.eq(
    "fs cp back: a nested file's content",
    Some("leaf".to_string()),
    std::fs::read_to_string(back.join("seed/sub/deep/leaf.txt")).ok().map(|s| s.trim().to_string()),
  );
  Ok(())
}
