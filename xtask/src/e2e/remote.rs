//! The harness on other machines over ssh.
//!
//! `--remote HOST` ships this commit (`git archive HEAD`) to one host that
//! builds natively, runs its cells there and shows the result here.
//!
//! `--hosts FILE` runs on several hosts at once, each in a role, so every
//! platform builds its own binaries on its own CPUs:
//!
//! - `local`: this machine, as a plain `cargo xtask e2e`.
//! - `build`: gets this commit, builds needy, the guest and the images
//!   itself, runs its cells, and exports a static needy, a static harness and
//!   the images for the `run` hosts that name it in `from`.
//! - `run`: compiles nothing. It gets its build host's export (streamed
//!   through this machine with `tar` over ssh, the container images with
//!   `docker load`) and runs the static harness, as `user` when the ssh login
//!   is root, since a root daemon hides permission bugs.
//! - `runner`: a Windows machine that builds its own `needy.exe` and joins
//!   the `local` host's `runner/at=windows` cells as a runner (`--windows-runner`;
//!   its `dir` is one directory name under its home).
//!
//! The file is JSON and stays local (`.tmp/` is ignored); host names are
//! arguments, never part of the repository:
//!
//! ```json
//! { "hosts": [
//!   { "name": "mac", "role": "local" },
//!   { "name": "kvm", "role": "build", "ssh": "ALIAS", "dir": "needy-e2e", "parallel": 4 },
//!   { "name": "small", "role": "run", "ssh": "root@ALIAS", "from": "kvm",
//!     "user": "needy-e2e", "dir": "/home/needy-e2e/needy-e2e", "only": ["linux-container.runc/"] }
//! ] }
//! ```

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use super::{Args, Format};

/// Checks that `dir` is a directory the harness may replace on a remote
/// host: under its home (relative) or an absolute path at least two levels
/// deep, without `..`.
fn checked_dir(dir: &str) -> Result<&str> {
  let parts: Vec<&str> = dir.trim_start_matches('/').split('/').collect();
  let bad = dir.is_empty()
    || dir.starts_with('~')
    || parts.iter().any(|p| *p == ".." || *p == "." || p.is_empty())
    || (dir.starts_with('/') && parts.len() < 2);
  if bad {
    bail!("a remote directory must be under the remote home (such as needy-e2e) or an absolute path two levels deep, not {dir:?}");
  }
  Ok(dir)
}

/// `--only`, `--skip` and the other options a remote harness repeats.
fn forwarded(args: &Args, only: &[String], skip: &[String], parallel: usize) -> Vec<String> {
  let mut out = Vec::new();
  for p in only {
    out.extend(["--only".to_string(), p.clone()]);
  }
  for p in skip {
    out.extend(["--skip".to_string(), p.clone()]);
  }
  out.extend([
    "--parallel".to_string(),
    parallel.to_string(),
    "--timeout".to_string(),
    args.timeout.to_string(),
  ]);
  if args.list {
    out.push("--list".into());
  }
  if args.keep {
    out.push("--keep".into());
  }
  out
}

fn ssh(host: &str) -> Command {
  let mut command = Command::new("ssh");
  command.args(["-o", "BatchMode=yes", "-o", "ServerAliveInterval=30", host]);
  command
}

/// Runs `script` on `host` and returns its stdout; its stderr goes to the
/// harness's, prefixed by nothing (the remote harness labels its lines).
fn remote(host: &str, script: &str) -> Result<(i32, Vec<u8>)> {
  let out = ssh(host).arg(script).stdin(Stdio::null()).stderr(Stdio::inherit()).output().context("cannot run ssh")?;
  Ok((out.status.code().unwrap_or(1), out.stdout))
}

/// Ships `HEAD` to `host:DIR/src`.
fn ship(root: &Path, host: &str, dir: &str) -> Result<()> {
  let dirty = Command::new("git").current_dir(root).args(["status", "--porcelain", "--untracked-files=no"]).output()?;
  if !dirty.stdout.is_empty() {
    eprintln!("e2e: uncommitted changes are not shipped; {host} runs HEAD");
  }
  let mut archive = Command::new("git").current_dir(root).args(["archive", "--format=tar", "HEAD"]).stdout(Stdio::piped()).spawn()?;
  let script = format!("rm -rf {dir}/src && mkdir -p {dir}/src && tar -x -C {dir}/src");
  let mut unpack = ssh(host).arg(script).stdin(archive.stdout.take().context("no archive")?).spawn()?;
  let archived = archive.wait()?;
  let unpacked = unpack.wait()?;
  if !archived.success() || !unpacked.success() {
    bail!("cannot ship HEAD to {host}:{dir}/src");
  }
  Ok(())
}

/// `cargo xtask ARGS` in `dir/src` on a build host. A non-interactive ssh
/// shell may not have cargo on PATH; rustup's env file puts it there.
fn cargo_xtask(dir: &str, args: &[String]) -> String {
  format!("cd {dir}/src && {{ [ -f \"$HOME/.cargo/env\" ] && . \"$HOME/.cargo/env\"; true; }} && cargo xtask {}", shell_words::join(args))
}

pub fn run(root: &Path, host: &str, args: &Args) -> Result<i32> {
  let dir = checked_dir(&args.remote_dir)?;
  eprintln!("e2e: shipping HEAD to {host}:{dir}/src");
  ship(root, host, dir)?;
  let mut remote_args = vec!["e2e".to_string(), "--work-dir".into(), "../work".into()];
  if args.clean {
    remote_args.push("--clean".into());
  } else {
    remote_args.extend(["-o".into(), "json".into()]);
    remote_args.extend(forwarded(args, &args.only, &args.skip, args.parallel));
  }
  let (code, stdout) = remote(host, &cargo_xtask(dir, &remote_args))?;
  if !args.remote_keep {
    let (cleaned, _) = remote(host, &format!("rm -rf {dir}/src"))?;
    if cleaned != 0 {
      eprintln!("e2e: cannot remove {host}:{dir}/src");
    }
  }
  if args.clean {
    return Ok(code);
  }
  show(args.output, &[(host.to_string(), parse(&stdout, code))])?;
  Ok(code)
}

fn parse(stdout: &[u8], code: i32) -> Result<Value, String> {
  serde_json::from_slice(stdout).map_err(|_| format!("the harness printed no JSON (exit {code})"))
}

/// Prints each host's result, and for several hosts the totals.
fn show(format: Format, results: &[(String, Result<Value, String>)]) -> Result<()> {
  match format {
    Format::Json => {
      let doc: serde_json::Map<String, Value> =
        results.iter().map(|(name, r)| (name.clone(), r.clone().unwrap_or_else(|e| json!({ "error": e })))).collect();
      let doc = if results.len() == 1 {
        doc.into_iter().next().map(|(_, v)| v).unwrap_or_default()
      } else {
        json!({ "hosts": doc })
      };
      writeln!(std::io::stdout(), "{}", serde_json::to_string_pretty(&doc)?)?;
    }
    Format::Human => {
      for (name, result) in results {
        match result {
          Ok(doc) => super::summary::print_human(name, doc),
          Err(e) => println!("e2e on {name}: FAILED TO RUN: {e}"),
        }
        println!();
      }
      if results.len() > 1 {
        let sum =
          |key: &str| results.iter().filter_map(|(_, r)| r.as_ref().ok()).map(|d| d["totals"][key].as_u64().unwrap_or(0)).sum::<u64>();
        let broken = results.iter().filter(|(_, r)| r.is_err()).count();
        println!(
          "all hosts: {} passed, {} failed, {} not covered, {} hosts that did not run",
          sum("pass"),
          sum("fail"),
          sum("not_covered"),
          broken
        );
      }
    }
  }
  Ok(())
}

/// One host of a `--hosts` file.
#[derive(Debug, Clone)]
struct Host {
  name: String,
  role: Role,
  /// The ssh destination; none for `local`.
  ssh: String,
  dir: String,
  /// A `run` host's build host, by name.
  from: Option<String>,
  /// The user a `run` host's harness runs as when the login is root.
  user: Option<String>,
  only: Vec<String>,
  skip: Vec<String>,
  parallel: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
  Local,
  Build,
  Run,
  Runner,
}

fn hosts(file: &Path) -> Result<Vec<Host>> {
  let text = std::fs::read_to_string(file).with_context(|| format!("cannot read {}", file.display()))?;
  let doc: Value = serde_json::from_str(&text).with_context(|| format!("{} is not JSON", file.display()))?;
  let strings = |v: &Value| -> Vec<String> {
    v.as_array().map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect()).unwrap_or_default()
  };
  let mut out = Vec::new();
  for h in doc["hosts"].as_array().context("the hosts file has no \"hosts\" list")? {
    let name = h["name"].as_str().context("a host has no name")?.to_string();
    let role = match h["role"].as_str() {
      Some("local") => Role::Local,
      Some("build") => Role::Build,
      Some("run") => Role::Run,
      Some("runner") => Role::Runner,
      other => bail!("host {name}: role {other:?} is none of local, build, run, runner"),
    };
    let ssh = h["ssh"].as_str().unwrap_or("").to_string();
    if role != Role::Local && ssh.is_empty() {
      bail!("host {name}: a {role:?} host needs \"ssh\"");
    }
    let dir = h["dir"].as_str().unwrap_or("needy-e2e").to_string();
    checked_dir(&dir).with_context(|| format!("host {name}"))?;
    out.push(Host {
      name,
      role,
      ssh,
      dir,
      from: h["from"].as_str().map(str::to_string),
      user: h["user"].as_str().map(str::to_string),
      only: strings(&h["only"]),
      skip: strings(&h["skip"]),
      parallel: h["parallel"].as_u64().map(|n| n as usize),
    });
  }
  for h in out.iter().filter(|h| h.role == Role::Run) {
    let from = h.from.as_deref().with_context(|| format!("run host {} names no build host in \"from\"", h.name))?;
    if !out.iter().any(|b| b.name == from && b.role == Role::Build) {
      bail!("run host {}: {from} is not a build host", h.name);
    }
  }
  Ok(out)
}

pub fn run_hosts(root: &Path, file: &Path, args: &Args) -> Result<i32> {
  let hosts = hosts(file)?;
  // A runner host joins the local host's cells: only this machine holds
  // the ssh access it needs.
  // TODO(e2e-runner): a runner host for a build host's cells would need
  // that host to reach it over ssh too; not needed while the Windows runner
  // is the only one, and the local host reaches it.
  let runner = hosts.iter().find(|h| h.role == Role::Runner);
  let only = |h: &Host| {
    if h.only.is_empty() {
      args.only.clone()
    } else {
      h.only.clone()
    }
  };
  let skip = |h: &Host| {
    if h.skip.is_empty() {
      args.skip.clone()
    } else {
      h.skip.clone()
    }
  };
  let parallel = |h: &Host| h.parallel.unwrap_or(args.parallel);
  let exporting = |b: &Host| hosts.iter().any(|h| h.role == Role::Run && h.from.as_deref() == Some(b.name.as_str()));

  if args.clean {
    for h in &hosts {
      let result = match h.role {
        Role::Local => super::host::clean(&args.work_dir.clone().unwrap_or_else(|| root.join(".tmp").join("e2e"))),
        Role::Build => ship(root, &h.ssh, &h.dir)
          .and_then(|()| {
            remote(
              &h.ssh,
              &cargo_xtask(
                &h.dir,
                &[
                  "e2e".into(),
                  "--work-dir".into(),
                  "../work".into(),
                  "--clean".into(),
                ],
              ),
            )
          })
          .and_then(|_| remote(&h.ssh, &format!("rm -rf {}", h.dir)).map(|_| ())),
        Role::Run => remote(
          &h.ssh,
          &format!(
            "docker image rm {} {} {} 2>/dev/null; rm -rf {}",
            super::host::AGENTS_IMAGE,
            super::host::ACTOR_IMAGE,
            super::host::EXECUTOR_IMAGE,
            h.dir
          ),
        )
        .map(|_| ()),
        Role::Runner => Ok(()),
      };
      if let Err(e) = result {
        eprintln!("e2e: {}: cannot clean: {e:#}", h.name);
      }
    }
    return Ok(0);
  }

  // Phase 1: every build host gets this commit and, for its run hosts,
  // builds and exports; the local cells start at once.
  let results = std::sync::Mutex::new(Vec::<(String, Result<Value, String>, i32)>::new());
  let record = |name: &str, result: Result<(i32, Vec<u8>)>| {
    let (parsed, code) = match result {
      Ok((code, stdout)) => (parse(&stdout, code), code),
      Err(e) => (Err(format!("{e:#}")), 1),
    };
    results.lock().unwrap_or_else(|e| e.into_inner()).push((name.to_string(), parsed, code));
  };
  std::thread::scope(|scope| -> Result<()> {
    for h in hosts.iter().filter(|h| h.role == Role::Local) {
      let record = &record;
      scope.spawn(move || {
        let mut command = Command::new(std::env::current_exe().unwrap_or_else(|_| "cargo".into()));
        command.args(["e2e", "-o", "json"]).args(forwarded(args, &only(h), &skip(h), parallel(h)));
        if let Some(runner) = runner {
          command.args([
            "--windows-runner",
            &runner.ssh,
            "--windows-dir",
            &runner.dir,
          ]);
        }
        if let Some(work) = &args.work_dir {
          command.arg("--work-dir").arg(work);
        }
        let out = command.stdin(Stdio::null()).stderr(Stdio::inherit()).output();
        record(&h.name, out.map(|o| (o.status.code().unwrap_or(1), o.stdout)).map_err(anyhow::Error::from));
      });
    }
    let mut exported = std::collections::BTreeMap::new();
    std::thread::scope(|phase| {
      let handles: Vec<_> = hosts
        .iter()
        .filter(|h| h.role == Role::Build)
        .map(|h| {
          let export = exporting(h);
          (
            h,
            phase.spawn(move || -> Result<()> {
              eprintln!("e2e: {}: shipping HEAD", h.name);
              ship(root, &h.ssh, &h.dir)?;
              if export {
                let args = [
                  "e2e".to_string(),
                  "--work-dir".into(),
                  "../work".into(),
                  "--export".into(),
                  "../export".into(),
                ];
                let (code, _) = remote(&h.ssh, &cargo_xtask(&h.dir, &args))?;
                if code != 0 {
                  bail!("the export on {} failed (exit {code})", h.name);
                }
              }
              Ok(())
            }),
          )
        })
        .collect();
      for (h, handle) in handles {
        let result = handle.join().unwrap_or_else(|_| Err(anyhow::anyhow!("the build thread panicked")));
        exported.insert(h.name.clone(), result.map_err(|e| format!("{e:#}")));
      }
    });

    // Phase 2: build hosts run their cells; run hosts fetch their build
    // host's export and run theirs.
    for h in hosts.iter().filter(|h| h.role == Role::Build) {
      let record = &record;
      let shipped = exported.get(&h.name).cloned().unwrap_or(Ok(()));
      scope.spawn(move || {
        let result = shipped.map_err(anyhow::Error::msg).and_then(|()| {
          let mut a = vec![
            "e2e".to_string(),
            "--work-dir".into(),
            "../work".into(),
            "-o".into(),
            "json".into(),
          ];
          a.extend(forwarded(args, &only(h), &skip(h), parallel(h)));
          remote(&h.ssh, &cargo_xtask(&h.dir, &a))
        });
        record(&h.name, result);
      });
    }
    for h in hosts.iter().filter(|h| h.role == Role::Run) {
      let record = &record;
      let build = hosts.iter().find(|b| Some(b.name.as_str()) == h.from.as_deref()).expect("checked in hosts()");
      let ready = exported.get(&build.name).cloned().unwrap_or_else(|| Err("not exported".into()));
      scope.spawn(move || {
        let result = ready.map_err(|e| anyhow::anyhow!("build host {}: {e}", build.name)).and_then(|()| fetch(build, h)).and_then(|()| {
          let work = format!("{}/work", h.dir);
          let mut a = vec![
            "e2e".to_string(),
            "--needy".into(),
            format!("{work}/bin/needy"),
            "--work-dir".into(),
            work.clone(),
            "--no-prepare".into(),
            "-o".into(),
            "json".into(),
          ];
          a.extend(forwarded(args, &only(h), &skip(h), parallel(h)));
          let harness = format!("{work}/bin/xtask {}", shell_words::join(&a));
          let script = match &h.user {
            // `runuser` (util-linux) drops to the test user with its own
            // groups (docker, kvm) and environment.
            Some(user) => format!("cd {} && runuser -l {user} -c {}", h.dir, shell_words::quote(&harness)),
            None => format!("cd {} && {harness}", h.dir),
          };
          remote(&h.ssh, &script)
        });
        record(&h.name, result);
      });
    }
    Ok(())
  })?;

  if !args.remote_keep {
    for h in hosts.iter().filter(|h| h.role == Role::Build) {
      if let Err(e) = remote(&h.ssh, &format!("rm -rf {0}/src {0}/export", h.dir)) {
        eprintln!("e2e: {}: cannot remove the shipped tree: {e:#}", h.name);
      }
    }
  }
  let mut results = results.into_inner().unwrap_or_else(|e| e.into_inner());
  results.sort_by_key(|(name, _, _)| hosts.iter().position(|h| &h.name == name));
  let code = results
    .iter()
    .map(|(_, r, code)| match r {
      Err(_) => 1,
      Ok(_) => *code,
    })
    .max_by_key(|c| match c {
      1 => 3,
      2 => 2,
      0 => 0,
      _ => 1,
    })
    .unwrap_or(0);
  let shown: Vec<(String, Result<Value, String>)> = results.into_iter().map(|(n, r, _)| (n, r)).collect();
  show(args.output, &shown)?;
  Ok(code)
}

/// Whether the run host has an executor image of the harness, whatever its
/// ID.
fn executor_present(run: &Host) -> Result<bool> {
  let (code, _) = remote(&run.ssh, &format!("docker image inspect {} >/dev/null 2>&1", super::host::EXECUTOR_IMAGE))?;
  Ok(code == 0)
}

/// Streams `build`'s export into `run`'s work directory, loads the
/// container images there, and gives the directory to the test user.
fn fetch(build: &Host, run: &Host) -> Result<()> {
  let arch = |h: &Host| remote(&h.ssh, "uname -m").map(|(_, out)| String::from_utf8_lossy(&out).trim().to_string());
  let (theirs, ours) = (arch(build)?, arch(run)?);
  if theirs != ours {
    bail!("{} builds for {theirs}, {} is {ours}", build.name, run.name);
  }
  // Only what the run host lacks: the container images it has under the
  // same IDs stay, and the VM image goes only where there is a KVM device.
  let (_, have) = remote(
    &run.ssh,
    &format!(
      "docker image inspect --format '{{{{.Id}}}}' {} {} 2>/dev/null; [ -e /dev/kvm ] && echo kvm; true",
      super::host::ACTOR_IMAGE,
      super::host::EXECUTOR_IMAGE
    ),
  )?;
  let have = String::from_utf8_lossy(&have).to_string();
  let work = format!("{}/work", run.dir);
  let (_, offered) = remote(&build.ssh, &format!("ls {0}/export/containers; cat {0}/export/images.hash 2>/dev/null", build.dir))?;
  let offered = String::from_utf8_lossy(&offered).to_string();
  let (_, images_hash) = remote(&run.ssh, &format!("cat {work}/images.hash 2>/dev/null; true"))?;
  let images_hash = String::from_utf8_lossy(&images_hash).trim().to_string();
  let mut parts = vec!["bin".to_string()];
  // The base VM image only where it boots, and only when it changed.
  let offered_hash = offered.lines().find(|l| !l.ends_with(".tar")).unwrap_or("").trim().to_string();
  if have.lines().any(|l| l == "kvm") && (offered_hash.is_empty() || offered_hash != images_hash) {
    parts.push("images".into());
    parts.push("images.hash".into());
  }
  // NOTICE: the executor image differs from the last one only in its needy
  // layer, which this commit changes every time, and sending all of it
  // through this machine's uplink took most of a run (2026-11-13). A run
  // host that has an executor image already gets it rebuilt from the
  // static needy sent anyway (below), which compiles nothing.
  let mut rebuild_executor = false;
  for archive in offered.split_whitespace().filter(|a| a.ends_with(".tar")) {
    let (role, id) = archive.trim_end_matches(".tar").split_once('-').unwrap_or(("", archive));
    if have.contains(id) {
      continue;
    }
    if role == "executor" && executor_present(run)? {
      rebuild_executor = true;
      continue;
    }
    parts.push(format!("containers/{archive}"));
  }
  eprintln!("e2e: {}: fetching {} of the export of {}", run.name, parts.join(", "), build.name);
  let replaced = if parts.iter().any(|p| p == "images") {
    format!("{work}/images {work}/images.hash")
  } else {
    String::new()
  };
  // NOTICE: the stream goes through this machine, whose uplink is the
  // bottleneck to far hosts; `gzip -1` trades a little CPU for about half
  // the bytes (the sparse rootfs and image layers compress well).
  let mut pack =
    ssh(&build.ssh).arg(format!("tar -C {}/export -c {} | gzip -1", build.dir, shell_words::join(&parts))).stdout(Stdio::piped()).spawn()?;
  let mut unpack = ssh(&run.ssh)
    .arg(format!("rm -rf {work}/bin {work}/guest {replaced} {work}/containers && mkdir -p {work} && gzip -dc | tar -x -C {work}"))
    .stdin(pack.stdout.take().context("no export stream")?)
    .spawn()?;
  let (packed, unpacked) = (pack.wait()?, unpack.wait()?);
  if !packed.success() || !unpacked.success() {
    bail!("cannot copy the export of {} to {}", build.name, run.name);
  }
  let mut script = format!(
    "mkdir -p {work}/guest && cp {work}/bin/needy {work}/guest/needy \
     && for f in {work}/containers/*.tar; do [ -f \"$f\" ] || continue; docker load -i \"$f\" || exit 1; done \
     && rm -rf {work}/containers"
  );
  if rebuild_executor {
    // The run host's executor image with this commit's needy in it.
    let image = super::host::EXECUTOR_IMAGE;
    script.push_str(&format!(
      " && mkdir -p {work}/executor && cp {work}/bin/needy {work}/executor/needy \
       && printf 'FROM {image}\\nUSER root\\nCOPY needy /usr/local/bin/needy\\nUSER agent\\n' > {work}/executor/Dockerfile \
       && docker build -q -t {image} {work}/executor >/dev/null && rm -rf {work}/executor"
    ));
  }
  if let Some(user) = &run.user {
    script.push_str(&format!(" && chown -R {user}: {}", run.dir));
  }
  let (code, _) = remote(&run.ssh, &script)?;
  if code != 0 {
    bail!("cannot load the container images on {}", run.name);
  }
  Ok(())
}
