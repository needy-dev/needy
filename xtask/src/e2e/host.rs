//! What this host has, found the way a runner finds its capabilities
//! (`needy_daemon::capabilities::probe`), and the images and guest needy
//! the harness builds from this tree before any cell runs.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use needy_config::profile::{ActorDriver, ExecutorDriver};
use sha2::{Digest, Sha256};

use super::matrix::{Driver, Oci, Platform};
use super::proc;

/// The container images the harness builds and the profiles name. Their
/// own names, so a harness run never replaces the `needy-actor:latest` or
/// `needy-executor:latest` someone built by hand.
pub const ACTOR_IMAGE: &str = "needy-e2e-actor:latest";
pub const EXECUTOR_IMAGE: &str = "needy-e2e-executor:latest";

const BUILD: Duration = Duration::from_secs(3600);

/// What this host offers the cells.
#[derive(Debug)]
pub struct Host {
  /// The harness's (and so every daemon's) effective user.
  pub euid: u32,
  /// Each actor driver of the matrix: available, or why not.
  pub drivers: BTreeMap<Driver, Result<(), String>>,
  /// `executor(linux-container)` with the harness's executor image.
  pub container_executor: Result<(), String>,
  /// What a `linux-container` actor needs to mount a workspace from an
  /// executor: sshfs on this host, and `user_allow_other` for a daemon
  /// that is not root.
  pub host_sshfs: Result<(), String>,
  /// The guest needy built from this tree.
  pub guest: Result<PathBuf, String>,
  /// The base VM image of this host's VM driver.
  pub vm_image: Option<PathBuf>,
  /// Whether `needy run stop` has `--now`.
  pub stop_now: bool,
  /// The engine every daemon is given, when the harness found it through
  /// the Docker CLI's context.
  pub docker_host: Option<String>,
}

/// What [`prepare`] does with the guest needy and the images.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prepare {
  /// Build what is missing or stale for this tree.
  Build,
  /// Use what an earlier run built (`--no-prepare`), whatever tree it was.
  Reuse,
  /// Nothing will run that needs them (`--list`, or no actor cell).
  Skip,
}

/// What the harness found out while preparing, kept for the summary.
#[derive(Debug, Default)]
pub struct Notes(pub Vec<String>);

/// Finds `DOCKER_HOST` from the Docker CLI's current context when it is
/// unset, so the daemons reach the same engine as `docker` does (OrbStack
/// and Docker Desktop on macOS listen elsewhere than `/var/run/docker.sock`).
pub fn docker_host() -> Option<String> {
  if std::env::var_os("DOCKER_HOST").is_some() || !proc::on_path("docker") {
    return None;
  }
  let out = proc::simple(
    "docker",
    &[
      "context",
      "inspect",
      "--format",
      "{{.Endpoints.docker.Host}}",
    ],
    Duration::from_secs(10),
  )
  .ok()?;
  let host = out.stdout.trim().to_string();
  (out.ok() && !host.is_empty() && host != "unix:///var/run/docker.sock").then_some(host)
}

/// A digest of the files under `root` that `paths` name (files or
/// directories, walked in order), so a build is redone only when its
/// inputs changed. A tree from `git archive` has no `.git`, so the harness
/// hashes the contents instead of asking git.
pub fn tree_hash(root: &Path, paths: &[&str]) -> Result<String> {
  let mut hasher = Sha256::new();
  let mut files = Vec::new();
  for path in paths {
    collect(&root.join(path), &mut files)?;
  }
  files.sort();
  for file in files {
    let relative = file.strip_prefix(root).unwrap_or(&file);
    hasher.update(relative.to_string_lossy().as_bytes());
    hasher.update([0]);
    hasher.update(std::fs::read(&file).with_context(|| format!("cannot read {}", file.display()))?);
    hasher.update([0]);
  }
  Ok(hasher.finalize().iter().take(8).map(|b| format!("{b:02x}")).collect())
}

fn collect(path: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
  let meta = match std::fs::symlink_metadata(path) {
    Ok(meta) => meta,
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
    Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
  };
  if meta.is_dir() {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if matches!(name, "target" | ".tmp" | ".git" | "node_modules") {
      return Ok(());
    }
    for entry in std::fs::read_dir(path).with_context(|| format!("cannot list {}", path.display()))? {
      collect(&entry?.path(), files)?;
    }
  } else if meta.is_file() {
    files.push(path.to_path_buf());
  }
  Ok(())
}

/// The needy sources a guest needy or the executor image is built from.
const NEEDY_SOURCES: &[&str] = &[
  "Cargo.toml",
  "Cargo.lock",
  "rust-toolchain.toml",
  "crates",
  "proto",
  "images/Dockerfile",
  "images/stub",
];
/// What the base images are built from.
const IMAGE_SOURCES: &[&str] = &["images"];

/// Builds what the cells need from this tree, unless `skip`: the guest
/// needy, the base VM image of this host's VM driver, the container actor
/// image and the executor image. Then probes the host. Every failure ends
/// up as the reason of the cells that needed the missing piece.
pub fn prepare(root: &Path, work: &Path, needy: &Path, mode: Prepare, notes: &mut Notes) -> Result<Host> {
  let skip = mode != Prepare::Build;
  // Listing builds nothing and judges nothing it would build.
  let listing = mode == Prepare::Skip;
  let platform = Platform::this();
  let docker_host = std::env::var("DOCKER_HOST").ok();
  let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
  let engine = needy_core_executor_probe();
  let kvm = Path::new(needy_core_actor::driver::linux_firecracker::kvm::KVM);
  // Which drivers this host could run, whatever images it has: what
  // `needy image pull` asks before it fetches images.
  let mut could = BTreeMap::new();
  for driver in ActorDriver::ALL {
    let found = runtime.block_on(needy_core_actor::driver::detect_host(driver, kvm, &engine));
    could.insert(driver, found);
  }
  let docker_cli = proc::on_path("docker");
  // The engine itself, which builds the guest and images on every host
  // (on a Mac too, where it runs no container actor).
  let engine_answers = runtime.block_on(engine.answer());

  let needy_hash = tree_hash(root, NEEDY_SOURCES)?;
  let image_hash = tree_hash(root, IMAGE_SOURCES)?;
  let stamps = work.join("stamps");
  std::fs::create_dir_all(&stamps)?;
  let fresh = |name: &str, hash: &str| std::fs::read_to_string(stamps.join(name)).is_ok_and(|s| s.trim() == hash);
  let stamp = |name: &str, hash: &str| std::fs::write(stamps.join(name), hash);
  let needs_docker = |what: &str| -> Result<(), String> {
    if !docker_cli {
      return Err(format!("{what} is built with the docker CLI, which is not on PATH"));
    }
    engine_answers.clone().map_err(|e| format!("{what} is built through a Docker-compatible engine, which does not answer: {e}"))
  };

  // The guest needy: `make guest`, into the harness's own directory.
  let guest_dir = work.join("guest");
  let guest_path = guest_dir.join("needy");
  let guest = if listing || (fresh("guest", &needy_hash) && guest_path.exists()) {
    Ok(guest_path.clone())
  } else if skip {
    if guest_path.exists() {
      notes.0.push(format!("--no-prepare: the guest needy in {} is not from this tree", guest_dir.display()));
      Ok(guest_path.clone())
    } else {
      Err("--no-prepare and no guest needy was built before".to_string())
    }
  } else {
    needs_docker("the guest needy").and_then(|()| {
      eprintln!("e2e: building the guest needy from this tree");
      let mut command = docker(root);
      command.args([
        "build",
        "-f",
        "images/Dockerfile",
        "--target",
        "guest",
        "--output",
      ]);
      command.arg(format!("type=local,dest={}", guest_dir.display()));
      command.arg(".");
      built(command, "the guest needy").and_then(|()| stamp("guest", &needy_hash).map_err(|e| e.to_string())).map(|()| guest_path.clone())
    })
  };

  // The base VM image of this host's VM driver.
  let vm_driver = needy_core_actor::image::vm::host_driver();
  let vm_dir = work.join("images").join(vm_driver.as_str()).join("base");
  let vm_image = match could.get(&vm_driver) {
    Some(Ok(())) => {
      let stamp_name = format!("vm-{}", vm_driver.as_str());
      if !(fresh(&stamp_name, &image_hash) && vm_dir.join("manifest.json").exists()) && !skip {
        match needs_docker("the base VM image") {
          Ok(()) => {
            eprintln!("e2e: building the {} base VM image", vm_driver.as_str());
            let mut command = Command::new(needy);
            command.current_dir(root).env("NEEDY_REPO", root).args(["image", "build", "--out"]).arg(&vm_dir);
            match built(command, "the base VM image") {
              Ok(()) => stamp(&stamp_name, &image_hash)?,
              Err(e) => notes.0.push(e),
            }
          }
          Err(e) => notes.0.push(e),
        }
      }
      Some(vm_dir.clone())
    }
    _ => None,
  };

  // The container actor image: in the engine on Linux, in Apple's
  // `container` on a Mac.
  let mut actor_image: BTreeMap<ActorDriver, Result<(), String>> = BTreeMap::new();
  if platform == Platform::Linux && engine_answers.is_ok() {
    let result = if listing || (fresh("actor-container", &image_hash) && image_present(ACTOR_IMAGE)) {
      Ok(())
    } else if skip {
      image_present(ACTOR_IMAGE).then_some(()).ok_or_else(|| format!("--no-prepare and {ACTOR_IMAGE} is not in the engine"))
    } else {
      needs_docker("the container actor image").and_then(|()| {
        eprintln!("e2e: building {ACTOR_IMAGE}");
        let mut command = docker(root);
        command.args([
          "build",
          "-f",
          "images/Dockerfile",
          "--target",
          "actor-container",
          "-t",
          ACTOR_IMAGE,
          ".",
        ]);
        built(command, "the container actor image").and_then(|()| stamp("actor-container", &image_hash).map_err(|e| e.to_string()))
      })
    };
    actor_image.insert(ActorDriver::LinuxContainer, result);
  }
  if platform == Platform::Macos && could.get(&ActorDriver::MacosAppleContainer).is_some_and(Result::is_ok) {
    let result = if listing || (fresh("actor-apple-container", &image_hash) && apple_image_present(ACTOR_IMAGE)) {
      Ok(())
    } else if skip {
      apple_image_present(ACTOR_IMAGE).then_some(()).ok_or_else(|| format!("--no-prepare and {ACTOR_IMAGE} is not in Apple's container"))
    } else {
      needs_docker("the container actor image").and_then(|()| {
        eprintln!("e2e: building {ACTOR_IMAGE} for Apple's container");
        let mut command = docker(root);
        command.args([
          "build",
          "-f",
          "images/Dockerfile",
          "--platform",
          "linux/arm64",
          "--target",
          "actor-container",
          "-t",
          ACTOR_IMAGE,
          ".",
        ]);
        built(command, "the container actor image")?;
        let archive = work.join("actor-image.tar");
        let mut save = docker(root);
        save.args(["save", ACTOR_IMAGE, "-o"]).arg(&archive);
        built(save, "the container actor image's archive")?;
        let mut load = Command::new("container");
        load.args(["image", "load", "--input"]).arg(&archive);
        let loaded = built(load, "the container actor image in Apple's container");
        // The archive is only the way into Apple's store.
        let _ = std::fs::remove_file(&archive);
        loaded?;
        // The engine's copy only fed the archive.
        let _ = proc::simple("docker", &["image", "rm", ACTOR_IMAGE], Duration::from_secs(60));
        stamp("actor-apple-container", &image_hash).map_err(|e| e.to_string())
      })
    };
    actor_image.insert(ActorDriver::MacosAppleContainer, result);
  }

  // The executor image, on Linux, where container executors run.
  let executor_image = if platform == Platform::Linux {
    if listing || (fresh("executor", &needy_hash) && image_present(EXECUTOR_IMAGE)) {
      Ok(())
    } else if skip {
      image_present(EXECUTOR_IMAGE).then_some(()).ok_or_else(|| format!("--no-prepare and {EXECUTOR_IMAGE} is not in the engine"))
    } else {
      needs_docker("the executor image").and_then(|()| {
        eprintln!("e2e: building {EXECUTOR_IMAGE} from this tree");
        let mut command = docker(root);
        command.args([
          "build",
          "-f",
          "images/Dockerfile",
          "--target",
          "executor",
          "-t",
          EXECUTOR_IMAGE,
          ".",
        ]);
        built(command, "the executor image").and_then(|()| stamp("executor", &needy_hash).map_err(|e| e.to_string()))
      })
    }
  } else {
    Err("container executors run on Linux only".into())
  };

  // Now what a runner would report, with the VM image just built.
  let facts = runtime.block_on(needy_daemon::capabilities::probe(kvm, vm_image.as_deref().unwrap_or(Path::new(""))));
  let mut drivers = BTreeMap::new();
  for driver in Driver::ACTORS {
    let profile_driver = match driver {
      Driver::Firecracker => ActorDriver::LinuxFirecracker,
      Driver::Container(_) => ActorDriver::LinuxContainer,
      Driver::Vz => ActorDriver::MacosVz,
      Driver::AppleContainer => ActorDriver::MacosAppleContainer,
      Driver::Host => continue,
    };
    let mut found = facts
      .actor
      .get(&profile_driver)
      .cloned()
      .unwrap_or_else(|| Err("not probed".into()))
      .map_err(|e| format!("actor({}): {e}", profile_driver.as_str()));
    if found.is_ok() {
      found = guest.clone().map(|_| ()).map_err(|e| format!("no guest needy: {e}"));
    }
    if found.is_ok()
      && let Some(image) = actor_image.get(&profile_driver)
    {
      found = image.clone().map_err(|e| format!("no container actor image: {e}"));
    }
    if found.is_ok()
      && let Driver::Container(oci) = driver
    {
      found = oci_runtime(oci);
    }
    drivers.insert(driver, found);
  }
  let container_executor = facts
    .executor
    .get(&ExecutorDriver::LinuxContainer)
    .cloned()
    .unwrap_or_else(|| Err("not probed".into()))
    .map_err(|e| format!("executor(linux-container): {e}"))
    .and(executor_image.map_err(|e| format!("no executor image: {e}")));
  let host_sshfs = if platform != Platform::Linux {
    Err("host-side sshfs mounts are a Linux container actor's".into())
  } else if !proc::on_path("sshfs") {
    Err("sshfs is not installed on this host (a linux-container actor mounts the workspace on the host)".into())
  } else if nix::unistd::geteuid().as_raw() != 0 && !allows_other() {
    Err("/etc/fuse.conf does not enable user_allow_other, which a daemon that is not root needs for the actor's sshfs mount".into())
  } else {
    Ok(())
  };
  let stop_now = proc::simple(&needy.to_string_lossy(), &["run", "stop", "--help"], Duration::from_secs(10))
    .map(|out| out.stdout.contains("--now"))
    .unwrap_or(false);
  if !stop_now {
    notes.0.push("`needy run stop` has no --now in this tree: the stop-now cells are not covered".into());
  }
  Ok(Host {
    euid: nix::unistd::geteuid().as_raw(),
    drivers,
    container_executor,
    host_sshfs,
    guest,
    vm_image,
    stop_now,
    docker_host,
  })
}

/// The engine probe the drivers share, as the daemon builds it.
fn needy_core_executor_probe() -> needy_core_executor::engine::Probe {
  needy_core_executor::engine::Probe::default()
}

fn docker(root: &Path) -> Command {
  let mut command = Command::new("docker");
  command.current_dir(root);
  command
}

/// Runs a build step; its output goes to the harness's stderr only when
/// it fails, as the reason.
fn built(command: Command, what: &str) -> Result<(), String> {
  match proc::run(command, BUILD) {
    Ok(out) if out.ok() => Ok(()),
    Ok(out) => {
      let tail: Vec<&str> = out.stderr.lines().chain(out.stdout.lines()).filter(|l| !l.trim().is_empty()).collect();
      let tail = tail[tail.len().saturating_sub(5)..].join(" | ");
      Err(format!("cannot build {what}: {tail}"))
    }
    Err(e) => Err(format!("cannot build {what}: {e:#}")),
  }
}

fn image_present(image: &str) -> bool {
  proc::simple("docker", &["image", "inspect", image], Duration::from_secs(30)).is_ok_and(|o| o.ok())
}

/// The engine's ID of `image`, if it has the image.
fn image_id(image: &str) -> Option<String> {
  let out = proc::simple("docker", &["image", "inspect", "--format", "{{.Id}}", image], Duration::from_secs(30)).ok()?;
  out.ok().then(|| out.stdout.trim().to_string())
}

fn apple_image_present(image: &str) -> bool {
  proc::simple("container", &["image", "inspect", image], Duration::from_secs(30)).is_ok_and(|o| o.ok())
}

/// Whether the engine has `oci`, and for gVisor whether it lets
/// containers reach host Unix sockets (what the driver's `check_host`
/// refuses otherwise).
fn oci_runtime(oci: Oci) -> Result<(), String> {
  let out = proc::simple("docker", &["info", "--format", "{{json .Runtimes}}"], Duration::from_secs(30)).map_err(|e| format!("{e:#}"))?;
  let runtimes: serde_json::Value =
    serde_json::from_str(out.stdout.trim()).map_err(|e| format!("cannot read the engine's runtimes: {e}"))?;
  let Some(runtime) = runtimes.get(oci.name()) else {
    let known: Vec<&str> = runtimes.as_object().map(|o| o.keys().map(String::as_str).collect()).unwrap_or_default();
    return Err(format!("the engine has no {} runtime (it has {})", oci.name(), known.join(", ")));
  };
  if oci == Oci::Runsc {
    let args = runtime.get("runtimeArgs").and_then(|a| a.as_array()).cloned().unwrap_or_default();
    if !args.iter().any(|a| a.as_str() == Some("--host-uds=all")) {
      return Err("runsc is registered without --host-uds=all, which container actors need".into());
    }
  }
  Ok(())
}

/// Whether `/etc/fuse.conf` enables `user_allow_other` on a line of its own.
fn allows_other() -> bool {
  // NOTICE: `/etc/fuse.conf` is the path libfuse reads (fuse(8), "CONFIGURATION").
  std::fs::read_to_string("/etc/fuse.conf").is_ok_and(|s| s.lines().any(|l| l.trim() == "user_allow_other"))
}

/// Writes what a Linux host without a toolchain needs to run cells, laid
/// out as a work directory: `bin/needy` and `bin/xtask` (static, this
/// tree's; the static needy is also the guest needy), the base VM image
/// under `images/`, and `containers/ID.tar` for the container actor and
/// executor images.
pub fn export(root: &Path, host: &Host, out: &Path) -> Result<()> {
  let bin = out.join("bin");
  std::fs::create_dir_all(&bin)?;
  eprintln!("e2e: building the static needy and harness for export");
  let mut command = docker(root);
  command.args([
    "build",
    "-f",
    "images/Dockerfile",
    "--target",
    "e2e",
    "--output",
  ]);
  command.arg(format!("type=local,dest={}", bin.display()));
  command.arg(".");
  built(command, "the static needy and harness").map_err(anyhow::Error::msg)?;
  if let Some(image) = &host.vm_image
    && image.join("manifest.json").exists()
  {
    let driver = needy_core_actor::image::vm::host_driver();
    let dest = out.join("images").join(driver.as_str());
    std::fs::create_dir_all(&dest)?;
    // `cp -R` keeps the sparse rootfs cheap to copy where the filesystem
    // supports it, and needs no copy of a directory walker here.
    let mut copy = Command::new("cp");
    copy.arg("-R").arg(image).arg(dest.join("base"));
    built(copy, "the base VM image's copy").map_err(anyhow::Error::msg)?;
  }
  // One archive per image, named by its ID, so a run host that has an
  // image already is not sent it again.
  let containers = out.join("containers");
  std::fs::create_dir_all(&containers)?;
  for image in [ACTOR_IMAGE, EXECUTOR_IMAGE] {
    let Some(id) = image_id(image) else {
      continue;
    };
    let mut save = docker(root);
    save.args(["save", image, "-o"]).arg(containers.join(format!("{}.tar", id.trim_start_matches("sha256:"))));
    built(save, "a container image's archive").map_err(anyhow::Error::msg)?;
  }
  eprintln!("e2e: exported to {}", out.display());
  Ok(())
}

/// Removes what the harness built: its container images and work
/// directory. Leaves everything else alone.
pub fn clean(work: &Path) -> Result<()> {
  if proc::on_path("docker") {
    for image in [ACTOR_IMAGE, EXECUTOR_IMAGE] {
      if image_present(image) {
        let out = proc::simple("docker", &["image", "rm", image], Duration::from_secs(120))?;
        if !out.ok() {
          bail!("cannot remove {image}: {}", out.brief());
        }
        eprintln!("e2e: removed {image}");
      }
    }
  }
  if proc::on_path("container") && apple_image_present(ACTOR_IMAGE) {
    let out = proc::simple("container", &["image", "delete", ACTOR_IMAGE], Duration::from_secs(120))?;
    if !out.ok() {
      bail!("cannot remove {ACTOR_IMAGE} from Apple's container: {}", out.brief());
    }
    eprintln!("e2e: removed {ACTOR_IMAGE} from Apple's container");
  }
  if work.exists() {
    std::fs::remove_dir_all(work).with_context(|| format!("cannot remove {}", work.display()))?;
    eprintln!("e2e: removed {}", work.display());
  }
  Ok(())
}
