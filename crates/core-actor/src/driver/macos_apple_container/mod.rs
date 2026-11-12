//! The `macos-apple-container` actor driver: the actor in a container that
//! Apple's `container` CLI runs on a Mac with Apple silicon, each container
//! in its own lightweight VM (Virtualization.framework, Apple's default
//! Linux kernel, `vminitd` as the VM's init).
//!
//! The driver drives the CLI; the process is the boundary tests stand in
//! for ([`Cli`]). It uses the `linux-container` driver's image, shims and channel
//! protocol, with the transport Apple's `container` offers:
//!
//! - the daemon's guest and gateway sockets are mounted into the container
//!   one file each (`-v host.sock:/.needy/channels/guest.sock`), which the
//!   CLI relays into the VM over vsock; a mounted directory of sockets does
//!   not work there;
//! - init's process socket is published (`--publish-socket`): the CLI
//!   listens on the host path and connects to init's socket for each
//!   connection.
//!
//! The container has no network but loopback (`--network none`). Init
//! mounts the workspace inside with sshfs, as a VM's does, and sets the
//! recipe's hostname ([`FACETS`]); it may, since the container
//! has `CAP_SYS_ADMIN` in its own VM. The CLI's output is the actor's log.
//!
//! See `docs/ai/references/architecture/2026-11-10-container-actor-runtime-design.md`
//! and the evidence in
//! `docs/ai/references/architecture/2026-11-12-macos-apple-container-actor-evidence.md`.

use std::path::Path;
use std::process::ExitStatus;
use std::sync::Arc;
use std::time::Duration;

use needy_config::profile::{Actor, ActorDriver, CONTAINER_IMAGE, ContainerActor};
use needy_config::recipe::IdentityField;
use needy_core::recipe::CompiledRecipe;
use needy_core::types::Capture;

mod cli;

pub use cli::{Cli, Output, PROGRAM, Process};

use crate::contract::{CHANNELS, GATEWAY_SOCKET, GUEST, GUEST_SOCKET, INIT, PROCESS_SOCKET, channels_dir};
use crate::driver::{Channels, Driver, Endpoint, Error, Facets, Gap, Identity, Leftover, Running, Side, Start, Terminal, Transport};
use crate::image::base;
use crate::image::container::name;

/// What builds the container actor image of this needy's version into
/// Apple's `container`.
const REBUILD: &str = "needy build --macos-apple-container";

/// What a `macos-apple-container` actor presents: the container image's
/// shims catch execs; init mounts the workspace and sets the hostname
/// inside, as a VM's does, but Apple's default kernel has no sensors and no
/// uptime offset; its guest and gateway sockets are mounted one file each,
/// and its process socket is published.
// TODO(macos-apple-container-kernel): the sensors and the uptime offset come
// with a needy-built arm64 kernel (`-k`), a separate approved slice.
pub const FACETS: Facets = Facets {
  capture: Capture::Shims,
  workspace: Side::Guest,
  identity: Identity {
    hostname: Some(Side::Guest),
    hwmon: None,
    uptime: None,
  },
  transport: Transport::Socket,
};

// NOTICE: `container system status` answers in about 50 ms on an Apple
// silicon laptop; 3 s bounds services that hang.
const DETECT_WAIT: Duration = Duration::from_secs(3);

/// Whether this host can run `macos-apple-container` actors: Apple's
/// `container` CLI is on `PATH` and its services answer. Whether its image
/// store has the container actor image is checked when a run is created.
pub async fn detect() -> Result<(), String> {
  // The image does not matter for the services.
  let driver = MacosAppleContainer::new(CONTAINER_IMAGE.into());
  match tokio::time::timeout(DETECT_WAIT, driver.services()).await {
    Ok(services) => services.map_err(|e| e.to_string()),
    Err(_) => Err(format!("`container system status` did not answer within {DETECT_WAIT:?}")),
  }
}

/// Deletes the container a daemon that died left, found by its run's name;
/// its `container run` process ended with the old daemon or ends with the
/// deletion.
pub async fn reconcile(leftover: &Leftover<'_>) -> bool {
  // The image does not matter for deleting a container.
  let driver = MacosAppleContainer::new(CONTAINER_IMAGE.into());
  tracing::warn!("run {}: deleting its actor's container, which a restarted control plane does not adopt", leftover.run);
  driver.end_leftover(leftover.run).await
}

/// What Apple's `container` refused or failed, as the driver's error.
pub(crate) fn refused(message: String) -> Error {
  Error::driver(ActorDriver::MacosAppleContainer, message)
}

/// The `macos-apple-container` driver: actors in containers of `image`, which
/// must be in Apple's `container` image store.
pub struct MacosAppleContainer {
  pub image: String,
  cli: Arc<dyn Cli>,
}

/// One container about to be run: the CLI's arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
  pub name: String,
  pub args: Vec<String>,
  pub vcpus: u32,
  pub mem_mib: u32,
}

impl MacosAppleContainer {
  /// The driver on this machine's `container` CLI. Nothing runs until it
  /// is used; a CLI that is missing fails each use, `check_host` first.
  pub fn new(image: String) -> Self {
    Self::with_cli(image, Arc::new(cli::Command))
  }

  /// The driver of `actor`, whose `container:` options may name the image.
  pub fn of(actor: &Actor) -> Self {
    Self::new(ContainerActor::image_of(actor.container.as_ref()))
  }

  pub fn with_cli(image: String, cli: Arc<dyn Cli>) -> Self {
    Self { image, cli }
  }

  async fn run(&self, args: &[&str]) -> Result<Output, Error> {
    let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    self.cli.output(&args).await
  }

  /// Fails unless the CLI is there and its services answer, as `container
  /// system status` says; a runner reports `actor(macos-apple-container)` only
  /// then.
  pub async fn services(&self) -> Result<(), Error> {
    let status = self.run(&["system", "status"]).await?;
    if !status.success {
      return Err(refused(format!(
        "Apple's `container` services are not running ({}); start them with `container system start`",
        first_line(&status)
      )));
    }
    Ok(())
  }

  /// Loads the image archive at `archive` (an OCI or `docker save` tar)
  /// into Apple's `container` image store, as `needy build
  /// --macos-apple-container` does.
  pub async fn load(&self, archive: &Path) -> Result<(), Error> {
    let archive = archive.display().to_string();
    let loaded = self.run(&["image", "load", "--input", &archive]).await?;
    if !loaded.success {
      return Err(refused(format!("`container image load` failed: {}", first_line(&loaded))));
    }
    Ok(())
  }

  /// The CLI arguments that run the actor for `start`.
  pub fn launch(&self, start: &Start<'_>) -> Launch {
    let (vcpus, mem_mib) = crate::driver::size(start.recipe);
    let user = start.user;
    let channels = channels_dir(start.dir);
    let name = name(start.run);
    // The VM's CPUs are its own, but the CLI refuses more than the Mac has.
    let host_cpus = std::thread::available_parallelism().map_or(1, |n| n.get() as u32);
    let mut args: Vec<String> = vec![
      "run".into(),
      "--name".into(),
      name.clone(),
      "--label".into(),
      format!("needy.run={}", start.run),
      "--label".into(),
      "needy.role=actor".into(),
      "--network".into(),
      "none".into(),
      "--cpus".into(),
      vcpus.min(host_cpus).to_string(),
      "--memory".into(),
      format!("{mem_mib}M"),
      // Init mounts the workspace with FUSE and sets the hostname; the
      // driving program runs as the profile's user without capabilities.
      // The container is alone in its VM, so the VM is the boundary.
      "--cap-add".into(),
      "CAP_SYS_ADMIN".into(),
    ];
    // NOTICE: the CLI takes a tmpfs's mount options after a colon
    // (`--tmpfs PATH:OPTIONS`), as Docker's does,
    // https://github.com/apple/container/blob/a15b46097aa4c2325da13c1fb5192b9274aab083/Sources/Services/ContainerAPIService/Client/Parser.swift#L410-L418
    // (`container` 1.7.1). The home directory is the user's, as a VM's is.
    for (path, options) in [
      ("/tmp".to_string(), "mode=1777".to_string()),
      ("/var/tmp".to_string(), "mode=1777".to_string()),
      ("/run".to_string(), "mode=755".to_string()),
      (user.home(), format!("mode=755,uid={},gid={}", user.uid, user.gid)),
    ] {
      args.push("--tmpfs".into());
      args.push(format!("{path}:{options}"));
    }
    for socket in [GUEST_SOCKET, GATEWAY_SOCKET] {
      args.push("--volume".into());
      args.push(format!("{}:{CHANNELS}/{socket}", channels.join(socket).display()));
    }
    // The guest needy goes in read-only over the base image's placeholder,
    // one file through virtiofs, as the sockets above.
    args.push("--volume".into());
    args.push(format!("{}:{GUEST}:ro", start.guest.display()));
    args.push("--publish-socket".into());
    args.push(format!("{}:{CHANNELS}/{PROCESS_SOCKET}", channels.join(PROCESS_SOCKET).display()));
    // NOTICE: the root file system stays writable (no `--read-only`, unlike
    // a Docker container actor's): `vminitd` reaches a published socket at
    // the container's root file system's path in the VM
    // (`socket.source = rootInGuest.appending(...)`,
    // https://github.com/apple/containerization/blob/baf212e5a8e432e539beee325050aa26443a154d/Sources/Containerization/LinuxContainer.swift#L1307-L1335),
    // outside the container's own mounts, so init's process socket must be
    // on the root file system itself and a tmpfs over `/.needy/channels`
    // hides it. Observed with `container` 1.7.1: under `--read-only` with a
    // tmpfs there, the host's connections closed at once. Each container
    // has its own copy of the image's file system, which ends with it.
    // TODO(macos-apple-container): a read-only root file system, once Apple's
    // `container` can publish a socket from a container's own mount.
    args.push("--entrypoint".into());
    args.push(INIT.into());
    // TODO(macos-apple-container-kernel): a needy-built arm64 kernel (`-k`) with
    // `binfmt_needy`, the fake sensors and DMI is a separate approved slice;
    // until it lands the actor runs Apple's default kernel.
    args.push(self.image.clone());
    Launch {
      name,
      args,
      vcpus,
      mem_mib,
    }
  }

  /// Ends the container a previous daemon started for run `run`. Says
  /// whether a container was there.
  pub async fn end_leftover(&self, run: &str) -> bool {
    let name = name(run);
    match self.run(&["delete", "--force", &name]).await {
      Ok(output) if output.success => true,
      Ok(output) => {
        tracing::debug!("run {run}: no container {name} to delete: {}", first_line(&output));
        false
      }
      Err(e) => {
        tracing::warn!("run {run}: cannot delete {name}: {e}");
        false
      }
    }
  }
}

/// The base ABI label of the linux/arm64 variant of an image, from what
/// `container image inspect` printed: `None` when it has no such label.
// NOTICE: the CLI prints a JSON array with one object per image, whose
// `variants` hold each platform's OCI image configuration as
// `config.config.Labels` (the OCI image config's `config.Labels`). Observed
// with `container` 1.7.1; the CLI documents no output format.
// Reference: https://github.com/opencontainers/image-spec/blob/v1.1.1/config.md#properties
fn base_abi(inspected: &str) -> Result<Option<String>, String> {
  let value: serde_json::Value = serde_json::from_str(inspected).map_err(|e| e.to_string())?;
  let variants = value.pointer("/0/variants").and_then(|v| v.as_array()).ok_or("it lists no image variants")?;
  let linux_arm64 = |variant: &&serde_json::Value| {
    variant.pointer("/config/os").and_then(|v| v.as_str()) == Some("linux")
      && variant.pointer("/config/architecture").and_then(|v| v.as_str()) == Some("arm64")
  };
  let variant = variants.iter().find(linux_arm64).or(variants.first()).ok_or("it lists no image variants")?;
  let label = variant.pointer("/config/config/Labels").and_then(|labels| labels.get(base::LABEL));
  Ok(label.and_then(|v| v.as_str()).map(String::from))
}

/// The first line of what a failed command said, for an error message.
fn first_line(output: &Output) -> String {
  let text = if output.stderr.trim().is_empty() {
    &output.stdout
  } else {
    &output.stderr
  };
  text.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or("no output").to_string()
}

/// What of `recipe` (or of a plain machine) an actor under Apple's
/// `container` with its default kernel does not present.
// TODO(macos-apple-container-kernel): DMI, the sensors and the uptime offset need
// a kernel with `CONFIG_DMI`, the fake coretemp and `CONFIG_TIME_NS`, which
// the needy-built arm64 kernel slice (`-k`) brings; the gaps below for them
// go with it.
fn gaps(recipe: Option<&CompiledRecipe>) -> Vec<Gap> {
  let mut gaps = vec![
    // NOTICE: `container` gives the VM one CPU more than asked for its own
    // agent (`cpus + cpuOverhead`,
    // https://github.com/apple/container/blob/a15b46097aa4c2325da13c1fb5192b9274aab083/Sources/Services/RuntimeLinux/Server/RuntimeService.swift#L268-L270);
    // `nproc` showed 3 for `--cpus 2`.
    Gap {
      field: None,
      message: "the VM has one CPU more than the recipe's (Apple's `container` adds one for its agent), and nproc counts it".into(),
    },
    Gap::of(IdentityField::Hwmon, "Apple's default kernel has no hwmon, so the actor has no sensors and hwmon faults have no effect"),
  ];
  let Some(recipe) = recipe else {
    return gaps;
  };
  let cpu = &recipe.cpu;
  if cpu.brand.is_some() || cpu.signature.is_some() || !cpu.hide.is_empty() || cpu.hypervisor.is_some() {
    // TODO(macos-apple-container): a recipe's /proc/cpuinfo could be mounted as a
    // file (that works), but its text is made from the host's first
    // processor block, which a Mac has none of, and an x86 text on an arm64
    // kernel would contradict `uname -m`; deferred until arm64 recipes or
    // the owner decide what an x86 recipe shows on a Mac.
    gaps.push(Gap::of(IdentityField::Cpuinfo, "/proc/cpuinfo is the arm64 VM's (Apple implementer 0x61), not the recipe's"));
    gaps.push(Gap::of(IdentityField::Cpuid, "the CPU is the Mac's Apple silicon core; there is no CPUID instruction to present"));
  }
  if recipe.identity.smbios.is_some() {
    gaps.push(Gap::of(IdentityField::Dmi, "Apple's default kernel has no DMI, so the actor has no /sys/class/dmi/id"));
  }
  if recipe.identity.has_pci() {
    gaps.push(Gap::of(IdentityField::Pci, "PCI IDs are the VM's virtio devices"));
  }
  if recipe.kernel_build_env.get("KERNELRELEASE").is_some_and(|r| !r.is_empty()) {
    gaps.push(Gap::of(IdentityField::Kernel, "uname and /proc/version show Apple's default kernel"));
  }
  if recipe.uptime_offset_secs != 0 {
    gaps.push(Gap::of(IdentityField::Uptime, "the uptime is the VM's, without the recipe's offset"));
  }
  gaps
}

#[async_trait::async_trait]
impl Driver for MacosAppleContainer {
  fn kind(&self) -> ActorDriver {
    ActorDriver::MacosAppleContainer
  }

  async fn check_host(&self) -> Result<(), Error> {
    self.services().await?;
    let image = self.run(&["image", "inspect", &self.image]).await?;
    if !image.success {
      return Err(refused(format!("the container actor image {} is not in Apple's `container` image store; run `{REBUILD}`", self.image)));
    }
    let abi = base_abi(&image.stdout).map_err(|e| refused(format!("cannot read `container image inspect {}`: {e}", self.image)))?;
    Ok(base::check(&self.image, abi.as_deref(), REBUILD)?)
  }

  fn channels(&self, dir: &Path) -> Channels {
    let channels = channels_dir(dir);
    let guest = channels.join(GUEST_SOCKET);
    let gateway = channels.join(GATEWAY_SOCKET);
    let process = channels.join(PROCESS_SOCKET);
    Channels {
      sockets: vec![guest.clone(), gateway.clone(), process.clone()],
      guest: Endpoint::Unix(guest),
      gateway: Endpoint::Unix(gateway),
      terminal: Terminal::Unix(process),
      gateway_port: 0,
      process_port: 0,
    }
  }

  fn identity_gaps(&self, recipe: Option<&CompiledRecipe>) -> Vec<Gap> {
    gaps(recipe)
  }

  async fn start(&self, start: &Start<'_>) -> Result<Box<dyn Running>, Error> {
    let launch = self.launch(start);
    // A container left by an earlier daemon would block the name.
    self.end_leftover(start.run).await;
    let log = crate::driver::log(start.dir);
    let file = std::fs::File::create(&log).map_err(|source| Error::Write { path: log, source })?;
    let process = self.cli.spawn(&launch.args, file)?;
    Ok(Box::new(Started {
      container: MacosAppleContainer {
        image: self.image.clone(),
        cli: self.cli.clone(),
      },
      run: start.run.to_string(),
      name: launch.name,
      process,
      vcpus: launch.vcpus,
      mem_mib: launch.mem_mib,
      ended: None,
    }))
  }
}

/// A running actor: the `container run` process, which ends when the
/// container does, with its exit code.
struct Started {
  container: MacosAppleContainer,
  run: String,
  name: String,
  process: Box<dyn Process>,
  vcpus: u32,
  mem_mib: u32,
  /// The status it ended with, once it was cleaned up.
  ended: Option<ExitStatus>,
}

impl Started {
  /// Deletes the container once its process ended with `status`.
  async fn finish(&mut self, status: ExitStatus) -> ExitStatus {
    if let Some(status) = self.ended {
      return status;
    }
    // Gone already when it exited normally; this covers a CLI that died.
    self.container.end_leftover(&self.run).await;
    self.ended = Some(status);
    status
  }
}

// NOTICE: how long a stop waits for `container run` to return after the
// container was stopped: the CLI tears the VM down first, which took under a
// second on an Apple silicon laptop; 10 s bounds a CLI that hangs.
const RUN_EXIT_WAIT: Duration = Duration::from_secs(10);

#[async_trait::async_trait]
impl Running for Started {
  /// The host PID of the `container run` process.
  fn pid(&self) -> u32 {
    self.process.id()
  }

  fn vcpus(&self) -> u32 {
    self.vcpus
  }

  fn mem_mib(&self) -> u32 {
    self.mem_mib
  }

  async fn wait(&mut self) -> std::io::Result<ExitStatus> {
    if let Some(status) = self.ended {
      return Ok(status);
    }
    let status = self.process.wait().await?;
    Ok(self.finish(status).await)
  }

  async fn stop(&mut self, grace: Duration) -> std::io::Result<ExitStatus> {
    if let Some(status) = self.ended {
      return Ok(status);
    }
    let seconds = grace.as_secs().to_string();
    match self.container.run(&["stop", "--time", &seconds, &self.name]).await {
      Ok(output) if !output.success => tracing::warn!("cannot stop {}: {}", self.name, first_line(&output)),
      Ok(_) => {}
      Err(e) => tracing::warn!("cannot stop {}: {e}", self.name),
    }
    let status = match tokio::time::timeout(grace + RUN_EXIT_WAIT, self.process.wait()).await {
      Ok(status) => status?,
      Err(_) => {
        tracing::warn!("`container run` for {} did not return within {:?} of the stop; killing it", self.name, grace + RUN_EXIT_WAIT);
        self.process.kill()?;
        self.process.wait().await?
      }
    };
    Ok(self.finish(status).await)
  }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
