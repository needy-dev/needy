//! `needy image pull`: the base images of a release, for the drivers this
//! host has.
//!
//! - A VM driver's base is the release asset
//!   `needy-actor-<driver>-<arch>.tar.gz`. Its `.sha256` is downloaded
//!   first, the archive is checked against it before anything is unpacked,
//!   and it is unpacked (holes kept) into a staging directory next to the
//!   images, whose manifest must be of the driver, its architecture and this
//!   needy's base ABI; then it is renamed into `images/<driver>/base`, so a
//!   daemon never boots a half-written image.
//! - A container driver's base, `ghcr.io/needy-dev/needy-actor:v<version>`,
//!   is pulled through the engine (with the executor image) or Apple's
//!   `container`, and its base ABI label checked.
//!
//! `--file` imports a VM image archive from this machine instead, for hosts
//! without a network.
// TODO(release-attestation): the release attests each archive's build
// provenance (`actions/attest-build-provenance` in release.yml), which `gh
// attestation verify` checks by hand; pull checks the SHA-256 only. Verifying
// the Sigstore bundle here is deferred (owner decision D2 of the actor images
// design) until the owner asks for it.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow, bail};
use needy_config::profile::{ActorDriver, ExecutorDriver};
use needy_core_actor::image::manifest::{self, Manifest};
use needy_core_actor::image::{base, container, vm};
use sha2::{Digest as _, Sha256};

use super::{is_vm, release_image};
use crate::cli::Context;
use crate::output;

/// Test hook, as `install/install.sh` has: a URL that serves the release
/// assets directly (such as a local HTTP server over a workflow run's
/// artifacts). It replaces the release's download URL, so `--version` names
/// only the version the container images are pulled for.
const DOWNLOAD_BASE: &str = "NEEDY_DOWNLOAD_BASE";

/// Where a release's assets are.
const RELEASES: &str = "https://github.com/needy-dev/needy/releases/download";

#[derive(clap::Args)]
pub struct Args {
  /// The drivers to pull the base for: linux-firecracker, macos-vz,
  /// linux-container (with the executor image), macos-apple-container.
  /// Every driver this host has when none is named.
  #[arg(long = "driver", value_name = "DRIVER", value_parser = super::driver)]
  drivers: Vec<ActorDriver>,
  /// The release to pull from, a needy version with or without its `v`.
  /// Defaults to this needy's.
  #[arg(long, value_name = "VERSION", conflicts_with = "file")]
  version: Option<String>,
  /// Import a VM image archive on this machine instead of downloading one,
  /// for a host without a network: a release's
  /// `needy-actor-<driver>-<arch>.tar.gz`. A `.sha256` next to it is
  /// checked when there is one.
  #[arg(long, value_name = "ARCHIVE")]
  file: Option<PathBuf>,
}

pub async fn run(args: Args, ctx: &Context) -> Result<i32> {
  let images = ctx.paths()?.images();
  if let Some(file) = &args.file {
    if args.drivers.len() > 1 || args.drivers.iter().any(|driver| !is_vm(*driver)) {
      bail!("--file imports one VM image archive; name its driver, linux-firecracker or macos-vz, with --driver, or none");
    }
    let dir = import(&images, args.drivers.first().copied(), file).await?;
    output::message(format_args!("image pull: imported {} into {}", file.display(), dir.display()));
    return Ok(0);
  }
  let version = match &args.version {
    Some(version) => version.strip_prefix('v').unwrap_or(version).to_string(),
    None => env!("CARGO_PKG_VERSION").to_string(),
  };
  // A `linux-container` actor's pull brings the executor image too; a host
  // with `linux-container` executors but no such actors pulls it alone.
  let (drivers, executor) = if args.drivers.is_empty() {
    let detected = detected().await?;
    let executor = detected.executor && !detected.actors.contains(&ActorDriver::LinuxContainer);
    (detected.actors, executor)
  } else {
    (args.drivers, false)
  };
  let client = reqwest::Client::builder().user_agent(concat!("needy/", env!("CARGO_PKG_VERSION"))).build()?;
  let mut failed = 0;
  for driver in drivers {
    let pulled = match driver {
      ActorDriver::LinuxFirecracker | ActorDriver::MacosVz => pull_vm(&client, &images, driver, &version, &assets(&version))
        .await
        .map(|dir| output::message(format_args!("image pull: {driver}: {}", dir.display()))),
      ActorDriver::LinuxContainer => pull_engine(&version).await,
      ActorDriver::MacosAppleContainer => pull_apple(&version).await,
    };
    if let Err(e) = pulled {
      output::message(format_args!("image pull: {driver}: {e:#}"));
      failed += 1;
    }
  }
  if executor && let Err(e) = pull_executor(&needy_core_executor::engine::connect()?, &version).await {
    output::message(format_args!("image pull: linux-container executors: {e:#}"));
    failed += 1;
  }
  Ok(if failed == 0 { 0 } else { 1 })
}

/// What this host has, as a runner's detection finds it.
struct Detected {
  /// The actor drivers, whatever images the host holds.
  actors: Vec<ActorDriver>,
  /// Whether it has `linux-container` executors.
  executor: bool,
}

/// The actor drivers this host has, whatever images it holds, and whether
/// it has `linux-container` executors.
async fn detected() -> Result<Detected> {
  let engine = needy_core_executor::engine::Probe::default();
  let kvm = Path::new(needy_core_actor::driver::linux_firecracker::KVM);
  let (mut found, mut missing) = (Vec::new(), Vec::new());
  for driver in ActorDriver::ALL {
    match needy_core_actor::driver::detect_host(driver, kvm, &engine).await {
      Ok(()) => found.push(driver),
      Err(why) => missing.push(format!("{driver}: {why}")),
    }
  }
  let executor = needy_core_executor::driver::detect(ExecutorDriver::LinuxContainer, &engine).await;
  for line in &missing {
    tracing::info!("not pulling for {line}");
  }
  if let Err(why) = &executor {
    tracing::info!("not pulling the executor image: {why}");
  }
  if found.is_empty() && executor.is_err() {
    bail!("this host has none of the actor drivers ({}); name one with --driver", missing.join("; "));
  }
  let mut names: Vec<&str> = found.iter().map(|driver| driver.as_str()).collect();
  if executor.is_ok() && !found.contains(&ActorDriver::LinuxContainer) {
    names.push("linux-container executors");
  }
  output::message(format_args!("image pull: for {}, the drivers this host has", names.join(", ")));
  Ok(Detected {
    actors: found,
    executor: executor.is_ok(),
  })
}

/// The release asset of `driver`'s base VM image.
fn archive_name(driver: ActorDriver) -> String {
  format!("needy-actor-{driver}-{}.tar.gz", vm::arch(driver))
}

/// Where release `version`'s assets are: the release's download URL, or
/// [`DOWNLOAD_BASE`] when it is set.
fn assets(version: &str) -> String {
  match std::env::var(DOWNLOAD_BASE) {
    Ok(base) if !base.is_empty() => base.trim_end_matches('/').to_string(),
    _ => format!("{RELEASES}/v{version}"),
  }
}

/// Downloads `driver`'s base VM image of release `version` from `assets`,
/// checks it and puts it in place; returns its directory.
async fn pull_vm(client: &reqwest::Client, images: &Path, driver: ActorDriver, version: &str, assets: &str) -> Result<PathBuf> {
  let name = archive_name(driver);
  let tag = format!("v{version}");
  let url = format!("{assets}/{name}");
  let expected = expected_sha256(client, &format!("{url}.sha256"), version, &name).await?;
  std::fs::create_dir_all(images).with_context(|| format!("cannot create {}", images.display()))?;
  let archive = images.join(format!(".needy-pull-{}.tar.gz", uuid::Uuid::new_v4()));
  let installed = async {
    output::message(format_args!("image pull: downloading {url}"));
    let actual = download(client, &url, &archive).await?;
    if actual != expected {
      bail!("{name} from {url} has SHA-256 {actual}, and its .sha256 says {expected}; nothing was installed");
    }
    output::message(format_args!("image pull: verified {name} (sha256 {actual})"));
    install(
      images,
      Some(driver),
      &archive,
      &actual,
      manifest::Source::Release {
        tag,
        url: url.clone(),
      },
    )
    .await
  }
  .await;
  remove_file(&archive);
  installed
}

/// The SHA-256 the release's `.sha256` asset at `url` says, as lowercase
/// hex. A release without it is most likely no release at all: a
/// development build's version.
async fn expected_sha256(client: &reqwest::Client, url: &str, version: &str, name: &str) -> Result<String> {
  let response = client.get(url).send().await.with_context(|| format!("cannot download {url}"))?;
  if response.status() == reqwest::StatusCode::NOT_FOUND {
    bail!(
      "release v{version} has no {name} ({url}: 404 Not Found). A needy without a release (a development build) has \
       no base to pull: build it in a needy checkout with `needy image build`, or pull another release's with `--version V`"
    );
  }
  let text = response.error_for_status().with_context(|| format!("cannot download {url}"))?.text().await?;
  // The file holds the bare hex digest, as the release workflow writes it;
  // a `sha256sum` line (digest, then the name) reads the same.
  let digest = text.split_whitespace().next().unwrap_or_default().to_ascii_lowercase();
  if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
    bail!("{url} holds no SHA-256: {text:?}");
  }
  Ok(digest)
}

/// Downloads `url` into `path`, and returns its SHA-256 as lowercase hex.
async fn download(client: &reqwest::Client, url: &str, path: &Path) -> Result<String> {
  use tokio::io::AsyncWriteExt as _;
  let mut response =
    client.get(url).send().await.and_then(reqwest::Response::error_for_status).with_context(|| format!("cannot download {url}"))?;
  let total = response.content_length();
  let mut file = tokio::fs::File::create(path).await.with_context(|| format!("cannot write {}", path.display()))?;
  let mut hasher = Sha256::new();
  let (mut received, mut shown) = (0u64, 0u64);
  while let Some(chunk) = response.chunk().await.with_context(|| format!("cannot download {url}"))? {
    hasher.update(&chunk);
    file.write_all(&chunk).await.with_context(|| format!("cannot write {}", path.display()))?;
    received += chunk.len() as u64;
    // NOTICE: a line every 32 MiB: an image archive is about 180 MiB, so a
    // slow download still shows it moves without flooding the terminal.
    if received - shown >= 32 << 20 {
      shown = received;
      match total {
        Some(total) => output::message(format_args!("image pull: {} of {} MiB", received >> 20, total >> 20)),
        None => output::message(format_args!("image pull: {} MiB", received >> 20)),
      }
    }
  }
  file.flush().await.with_context(|| format!("cannot write {}", path.display()))?;
  Ok(hex(&hasher.finalize()))
}

/// Imports the VM image archive `file`, of `driver` when it is named, after
/// checking it against the `.sha256` next to it when there is one.
async fn import(images: &Path, driver: Option<ActorDriver>, file: &Path) -> Result<PathBuf> {
  let file = std::fs::canonicalize(file).with_context(|| format!("cannot open {}", file.display()))?;
  let hashed = file.clone();
  let actual = tokio::task::spawn_blocking(move || sha256_of(&hashed)).await??;
  let mut sum = file.clone().into_os_string();
  sum.push(".sha256");
  match std::fs::read_to_string(&sum) {
    Ok(text) => {
      let expected = text.split_whitespace().next().unwrap_or_default().to_ascii_lowercase();
      if expected != actual {
        bail!("{} has SHA-256 {actual}, and {} says {expected}; nothing was installed", file.display(), PathBuf::from(&sum).display());
      }
      output::message(format_args!("image pull: verified {} (sha256 {actual})", file.display()));
    }
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
      output::message(format_args!("image pull: no .sha256 next to {}; its SHA-256 is {actual}", file.display()))
    }
    Err(e) => return Err(e).with_context(|| format!("cannot read {}", PathBuf::from(&sum).display())),
  }
  std::fs::create_dir_all(images).with_context(|| format!("cannot create {}", images.display()))?;
  install(images, driver, &file, &actual, manifest::Source::File { path: file.clone() }).await
}

/// The SHA-256 of the file at `path`, as lowercase hex.
fn sha256_of(path: &Path) -> Result<String> {
  let mut file = std::fs::File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
  let mut hasher = Sha256::new();
  std::io::copy(&mut file, &mut hasher).with_context(|| format!("cannot read {}", path.display()))?;
  Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
  bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Unpacks the VM image archive `archive` (gzip-compressed tar, with a
/// manifest from the release workflow) into a staging directory in `images`,
/// checks it is an image of `driver` (when named), of its architecture and
/// of this needy's base ABI, records `source` and `sha256` in its manifest,
/// and renames it into the driver's base directory. Returns that directory.
async fn install(images: &Path, driver: Option<ActorDriver>, archive: &Path, sha256: &str, source: manifest::Source) -> Result<PathBuf> {
  let staging = images.join(format!(".needy-pull-{}", uuid::Uuid::new_v4()));
  let (from, to) = (archive.to_path_buf(), staging.clone());
  let installed = async {
    tokio::task::spawn_blocking(move || -> Result<()> {
      std::fs::create_dir(&to).with_context(|| format!("cannot create {}", to.display()))?;
      let file = std::fs::File::open(&from).with_context(|| format!("cannot open {}", from.display()))?;
      super::build::unpack_from(flate2::read::GzDecoder::new(std::io::BufReader::new(file)), &to)
    })
    .await??;
    let mut manifest = manifest::read(&staging)?
      .with_context(|| format!("{} has no {}: it is not a needy VM image archive", archive.display(), manifest::FILE))?;
    check(&manifest, driver)?;
    manifest.source = Some(source);
    manifest.sha256 = Some(sha256.to_string());
    manifest::write(&staging, &manifest)?;
    let target = vm::base_dir(images, manifest.driver);
    // The image's files, as the driver checks them before it boots it.
    match manifest.driver {
      ActorDriver::MacosVz => vm::MacosVz::new(&staging).check()?,
      _ => vm::LinuxFirecracker::new(&staging).check()?,
    }
    replace(&staging, &target)?;
    Ok(target)
  }
  .await;
  if installed.is_err()
    && let Err(e) = std::fs::remove_dir_all(&staging)
    && e.kind() != std::io::ErrorKind::NotFound
  {
    tracing::warn!("cannot remove {}: {e}", staging.display());
  }
  installed
}

/// Fails unless `manifest` is of a VM image of `driver` (when named), of
/// that driver's architecture, and of this needy's base ABI.
fn check(manifest: &Manifest, driver: Option<ActorDriver>) -> Result<()> {
  let found = manifest.driver;
  if let Some(driver) = driver
    && driver != found
  {
    bail!("the archive holds a {found} image, not a {driver} one");
  }
  if !super::is_vm(found) {
    bail!("the archive's manifest names {found}, which boots no VM image");
  }
  if manifest.arch != vm::arch(found) {
    bail!("the archive's {found} image is for {}, and {found} actors run {}", manifest.arch, vm::arch(found));
  }
  if manifest.base_abi != base::ABI {
    bail!(
      "the {found} base of needy {} has base ABI {}, and this needy speaks base ABI {}; pull this needy's release (without \
       --version), or use a needy that speaks base ABI {}",
      manifest.version,
      manifest.base_abi,
      base::ABI,
      manifest.base_abi
    );
  }
  Ok(())
}

/// Puts the directory `staging` at `target`, which may hold an older image:
/// the older one is moved aside first and removed after, so `target` is
/// never half-written.
fn replace(staging: &Path, target: &Path) -> Result<()> {
  let parent = target.parent().context("the image directory has no parent")?;
  std::fs::create_dir_all(parent).with_context(|| format!("cannot create {}", parent.display()))?;
  let old = parent.join(format!(".needy-old-{}", uuid::Uuid::new_v4()));
  let moved = match std::fs::rename(target, &old) {
    Ok(()) => true,
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
    Err(e) => return Err(e).with_context(|| format!("cannot move {} aside", target.display())),
  };
  if let Err(e) = std::fs::rename(staging, target) {
    if moved && let Err(back) = std::fs::rename(&old, target) {
      tracing::warn!("cannot move {} back: {back}", old.display());
    }
    return Err(e).with_context(|| format!("cannot move the image to {}", target.display()));
  }
  if moved && let Err(e) = std::fs::remove_dir_all(&old) {
    tracing::warn!("cannot remove the older image {}: {e}", old.display());
  }
  Ok(())
}

/// Removes the file at `path`, which may be gone.
fn remove_file(path: &Path) {
  if let Err(e) = std::fs::remove_file(path)
    && e.kind() != std::io::ErrorKind::NotFound
  {
    tracing::warn!("cannot remove {}: {e}", path.display());
  }
}

/// Pulls the container actor base and the executor image of release
/// `version` through the engine, and checks the base's base ABI.
async fn pull_engine(version: &str) -> Result<()> {
  let engine = needy_core_executor::engine::connect()?;
  let actor = release_image("needy-actor", version);
  let mut failures = Vec::new();
  output::message(format_args!("image pull: pulling {actor}"));
  match engine.pull(&actor).await {
    Ok(()) => match engine.inspect_image(&actor).await? {
      Some(image) => match base::check(
        &actor,
        image.labels.get(base::LABEL).map(String::as_str),
        &base::rebuild_from("needy image build --linux-container", None),
      ) {
        Ok(()) => output::message(format_args!("image pull: linux-container: {actor} (base ABI {})", base::ABI)),
        Err(e) => failures.push(e.to_string()),
      },
      None => failures.push(format!("the engine pulled {actor} but does not have it")),
    },
    Err(e) => failures.push(container::unpullable(&actor, &e.to_string(), "needy image build --linux-container")),
  }
  if let Err(e) = pull_executor(&engine, version).await {
    failures.push(e.to_string());
  }
  if failures.is_empty() {
    Ok(())
  } else {
    Err(anyhow!(failures.join("; ")))
  }
}

/// Pulls the executor image of release `version` through `engine`.
async fn pull_executor(engine: &std::sync::Arc<dyn needy_core_executor::engine::Engine>, version: &str) -> Result<()> {
  let executor = release_image("needy-executor", version);
  output::message(format_args!("image pull: pulling {executor}"));
  match engine.pull(&executor).await {
    Ok(()) => {
      output::message(format_args!("image pull: linux-container executors: {executor}"));
      Ok(())
    }
    Err(e) => Err(anyhow!(
      "the executor image {executor} cannot be pulled ({e}); a needy without a release (a development build) has none to pull: \
       build it with `needy image build --executor` and start the daemon with `--executor-image {}`",
      needy_core_executor::driver::linux_container::BUILT_IMAGE
    )),
  }
}

/// Pulls the container actor base of release `version` into Apple's
/// `container`, and checks its base ABI.
#[cfg(unix)]
async fn pull_apple(version: &str) -> Result<()> {
  use needy_core_actor::driver::macos_apple_container::MacosAppleContainer;
  let actor = release_image("needy-actor", version);
  let store = MacosAppleContainer::new(actor.clone());
  output::message(format_args!("image pull: pulling {actor} into Apple's container"));
  if let Err(e) = store.pull().await {
    bail!(container::unpullable(&actor, &e.to_string(), "needy image build --macos-apple-container"));
  }
  let images = store.images().await?;
  let found = images.iter().find(|image| image.name == actor).and_then(|image| image.base_abi.clone());
  base::check(&actor, found.as_deref(), &base::rebuild_from("needy image build --macos-apple-container", None))?;
  output::message(format_args!("image pull: macos-apple-container: {actor} (base ABI {})", base::ABI));
  Ok(())
}

#[cfg(not(unix))]
async fn pull_apple(_version: &str) -> Result<()> {
  bail!("Apple's `container` runs on macOS only, on a Mac with Apple silicon")
}

#[cfg(test)]
#[path = "pull_test.rs"]
mod tests;
