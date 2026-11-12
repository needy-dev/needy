//! `needy build`: builds the actor image this host's VM driver boots (the
//! Firecracker actor's on Linux, the `macos-vz` actor's on Apple silicon macOS)
//! and, with `--linux-container` and `--executor`, the container actor and
//! executor images from `images/Dockerfile`, through the Docker API
//! with BuildKit. With `--macos-apple-container` it builds the container actor
//! image for linux/arm64 instead of the VM's, and loads it into Apple's
//! `container`.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use bollard::Docker;
use bollard::models::{BuildInfo, BuildInfoAux};
use bollard::query_parameters::{BuildImageOptions, BuilderVersion, ImageBuildOutput};
use futures_util::StreamExt;
use needy_config::Source;
use needy_config::validate::Severity;
use needy_core::recipe::PerRun;

use crate::cli::Context;
use crate::output;

/// Lines of a failed step's output to show with its error.
const FAILED_STEP_LINES: usize = 30;

#[derive(clap::Args)]
pub struct Args {
  /// Recipe whose kernel options the actor image is built with: a path, or
  /// `builtin:NAME`.
  #[arg(long)]
  recipe: Option<String>,
  /// Output directory for the actor image: vmlinux, rootfs.ext4 and
  /// firecracker, or on Apple silicon macOS Image, initrd and rootfs.ext4
  /// for `macos-vz` actors. Defaults to the image the daemon boots.
  #[arg(long)]
  out: Option<PathBuf>,
  /// Also build the `needy-executor` Docker image.
  #[arg(long)]
  executor: bool,
  /// Also build the `needy-actor` image, the `linux-container` actor's.
  #[arg(long)]
  linux_container: bool,
  /// Build the `needy-actor` image for linux/arm64 and load it into Apple's
  /// `container`, for `actor.driver: macos-apple-container` on a Mac with Apple
  /// silicon, instead of the VM actor image.
  #[arg(long, conflicts_with_all = ["recipe", "out"])]
  macos_apple_container: bool,
}

pub async fn run(args: Args, ctx: &Context) -> Result<i32> {
  let root = repo_root()?;
  let out = match args.out {
    Some(out) => out,
    None => ctx.paths()?.default_image(),
  };
  let mut build_args = HashMap::new();
  if let Some(reference) = &args.recipe {
    let recipe = needy_config::recipe::load(&Source::reference(reference, Path::new("")))?;
    // Only the kernel's make variables are used, which no per-run value
    // changes.
    let compiled = needy_core::recipe::compile(&recipe, &needy_core_actor::driver::linux_firecracker::kvm::detect()?, &PerRun::at(0, 0));
    for finding in &compiled.findings {
      output::message(format_args!("recipe {finding}"));
    }
    if compiled.findings.iter().any(|f| f.severity == Severity::Error) {
      bail!("the recipe has errors");
    }
    build_args.extend(compiled.kernel_build_env);
  }
  let context = tokio::task::spawn_blocking(move || pack_context(&root)).await??;
  let docker = Docker::connect_with_defaults().context("cannot reach Docker")?;
  if args.macos_apple_container {
    build_for_macos_apple_container(&docker, &context, &ctx.paths()?.state).await?;
  } else {
    build_vm_image(&docker, &context, &out, build_args).await?;
  }
  if args.linux_container {
    build(
      &docker,
      &context,
      BuildImageOptions {
        target: "actor-container".into(),
        t: Some(needy_config::profile::CONTAINER_IMAGE.into()),
        ..options()
      },
    )
    .await?;
  }
  if args.executor {
    build(
      &docker,
      &context,
      BuildImageOptions {
        target: "executor".into(),
        t: Some("needy-executor:latest".into()),
        ..options()
      },
    )
    .await?;
  }
  Ok(0)
}

/// The Dockerfile target of the actor image this host boots, and the
/// platform to build it for (empty for the builder's own).
// NOTICE: a Mac's Docker (Docker Desktop, OrbStack) builds in an arm64 Linux
// VM, so the `macos-vz` image builds natively there; `linux/arm64` is named so an
// x86 builder would fail instead of making an image the Mac cannot boot.
fn actor_target() -> (&'static str, &'static str) {
  if needy_core_actor::driver::macos_vz::supported_host() {
    ("actor-macos-vz", "linux/arm64")
  } else {
    ("actor", "")
  }
}

/// Builds the actor image this host's VM driver boots, with the recipe's
/// kernel `build_args`, into `out`.
async fn build_vm_image(docker: &Docker, context: &[u8], out: &Path, build_args: HashMap<String, String>) -> Result<()> {
  std::fs::create_dir_all(out).with_context(|| format!("cannot create {}", out.display()))?;
  let out = std::fs::canonicalize(out)?;
  // NOTICE: bollard's local exporter (`ImageBuildOutput::Local`, bollard
  // 0.21.1 `src/grpc/mod.rs`, `FileSendPacketImpl`) writes files without their
  // modes, so `firecracker` would lose its executable bit. The tar exporter
  // keeps them; the tar is unpacked here and removed. Remove this when the
  // local exporter applies `Stat.mode`.
  let archive = out.join(".needy-build.tar");
  let (target, platform) = actor_target();
  build(
    docker,
    context,
    BuildImageOptions {
      target: target.into(),
      platform: platform.into(),
      buildargs: Some(build_args),
      outputs: Some(ImageBuildOutput::Tar(archive.display().to_string())),
      ..options()
    },
  )
  .await?;
  let unpack_to = out.clone();
  let unpack_from = archive.clone();
  tokio::task::spawn_blocking(move || unpack(&unpack_from, &unpack_to)).await??;
  std::fs::remove_file(&archive).with_context(|| format!("cannot remove {}", archive.display()))?;
  Ok(())
}

/// Builds the container actor image for linux/arm64, the platform of Apple's
/// `container` on Apple silicon, and loads it into Apple's image store
/// through an archive in `scratch`, which is removed after.
// NOTICE: Docker builds it (Docker Desktop or OrbStack on the Mac, whose
// builder is arm64 natively); Apple's `container build` could build the
// Dockerfile without Docker, but needy's build drives BuildKit through the
// Docker API only.
// TODO(macos-apple-container): build with `container build` when there is no
// Docker, if the owner wants Macs without Docker to build the image.
async fn build_for_macos_apple_container(docker: &Docker, context: &[u8], scratch: &Path) -> Result<()> {
  let image = needy_config::profile::CONTAINER_IMAGE;
  build(
    docker,
    context,
    BuildImageOptions {
      target: "actor-container".into(),
      t: Some(image.into()),
      platform: "linux/arm64".into(),
      ..options()
    },
  )
  .await?;
  std::fs::create_dir_all(scratch).with_context(|| format!("cannot create {}", scratch.display()))?;
  let archive = scratch.join(format!(".needy-actor-{}.tar", uuid::Uuid::new_v4()));
  let loaded = match save_image(docker, image, &archive).await {
    Ok(()) => {
      output::message(format_args!("build: loading {image} into Apple's container"));
      load_into_apple_container(image, &archive).await
    }
    Err(e) => Err(e),
  };
  if let Err(e) = std::fs::remove_file(&archive)
    && e.kind() != std::io::ErrorKind::NotFound
  {
    tracing::warn!("cannot remove {}: {e}", archive.display());
  }
  loaded
}

#[cfg(unix)]
async fn load_into_apple_container(image: &str, archive: &Path) -> Result<()> {
  let driver = needy_core_actor::driver::macos_apple_container::MacosAppleContainer::new(image.into());
  Ok(driver.load(archive).await?)
}

#[cfg(not(unix))]
async fn load_into_apple_container(_image: &str, _archive: &Path) -> Result<()> {
  bail!("Apple's `container` runs on macOS only, on a Mac with Apple silicon")
}

/// Writes Docker's archive of `image`, as `docker save` writes it, to
/// `path`.
async fn save_image(docker: &Docker, image: &str, path: &Path) -> Result<()> {
  use tokio::io::AsyncWriteExt as _;
  let failed = || format!("cannot write {}", path.display());
  let mut file = tokio::fs::File::create(path).await.with_context(failed)?;
  let mut stream = docker.export_image(image);
  while let Some(chunk) = stream.next().await {
    let chunk = chunk.with_context(|| format!("cannot export {image} from Docker"))?;
    file.write_all(&chunk).await.with_context(failed)?;
  }
  file.flush().await.with_context(failed)?;
  Ok(())
}

fn options() -> BuildImageOptions {
  BuildImageOptions {
    dockerfile: "images/Dockerfile".into(),
    version: BuilderVersion::BuilderBuildKit,
    // BuildKit needs a client session; bollard serves it, and with it the
    // local output directory.
    session: Some(uuid::Uuid::new_v4().to_string()),
    ..Default::default()
  }
}

fn repo_root() -> Result<PathBuf> {
  if let Ok(root) = std::env::var("NEEDY_REPO") {
    return Ok(PathBuf::from(root));
  }
  let mut dir = std::env::current_dir()?;
  loop {
    if dir.join("images").join("Dockerfile").exists() {
      return Ok(dir);
    }
    if !dir.pop() {
      bail!("cannot find images/Dockerfile; set NEEDY_REPO to the needy checkout");
    }
  }
}

/// Packs the build context: the checkout without what `.dockerignore` and
/// `.gitignore` exclude.
///
/// NOTICE: the Docker CLI filters the context on the client, and the API
/// takes the tar as given. `ignore` reads `.dockerignore` with gitignore
/// rules, where a pattern without a slash matches at any depth instead of
/// only at the root; the patterns in this repository are all top-level names.
/// `.gitignore` is applied too, so local state such as `target/` and `.tmp/`
/// never enters the context.
fn pack_context(root: &Path) -> Result<Vec<u8>> {
  let mut tar = tar::Builder::new(Vec::new());
  tar.follow_symlinks(false);
  let walk = ignore::WalkBuilder::new(root)
    .hidden(false)
    .require_git(false)
    .git_global(false)
    .git_exclude(false)
    .add_custom_ignore_filename(".dockerignore")
    .build();
  for entry in walk {
    let entry = entry?;
    let relative = entry.path().strip_prefix(root)?;
    let Some(kind) = entry.file_type() else {
      continue;
    };
    if relative.as_os_str().is_empty() {
      continue;
    }
    if kind.is_dir() {
      tar.append_dir(relative, entry.path())?;
    } else if kind.is_file() || kind.is_symlink() {
      tar.append_path_with_name(entry.path(), relative)?;
    }
    // Sockets and other special files cannot be part of an image.
  }
  Ok(tar.into_inner()?)
}

/// Unpacks the actor image's tar into `out`, keeping file modes and leaving
/// runs of zeros in regular files as holes.
fn unpack(archive: &Path, out: &Path) -> Result<()> {
  let file = std::fs::File::open(archive).with_context(|| format!("cannot open {}", archive.display()))?;
  let mut tar = tar::Archive::new(std::io::BufReader::new(file));
  tar.set_preserve_permissions(true);
  tar.set_overwrite(true);
  let unpacking = || format!("cannot unpack the image into {}", out.display());
  for entry in tar.entries().with_context(unpacking)? {
    let mut entry = entry.with_context(unpacking)?;
    if entry.header().entry_type().is_file() {
      unpack_file(&mut entry, out).with_context(unpacking)?;
    } else {
      entry.unpack_in(out).with_context(unpacking)?;
    }
  }
  Ok(())
}

/// Writes one regular file of the image into `out`.
///
/// NOTICE: `tar::Entry::unpack_in` writes every byte, so the actor's
/// `rootfs.ext4` (an ext4 image whose free blocks are zeros) took its full
/// length on disk. BuildKit's tar exporter has no sparse entries to keep the
/// holes, so they are made here while writing. Regular files are written by
/// hand for that; everything else still goes through `unpack_in`.
fn unpack_file(entry: &mut tar::Entry<impl std::io::Read>, out: &Path) -> Result<()> {
  let entry_path = entry.path()?.into_owned();
  let mut path = out.to_path_buf();
  for component in entry_path.components() {
    match component {
      std::path::Component::Normal(part) => path.push(part),
      std::path::Component::CurDir => {}
      _ => bail!("the image holds a file outside its root: {}", entry_path.display()),
    }
  }
  if let Some(parent) = path.parent() {
    std::fs::create_dir_all(parent)?;
  }
  // As `set_overwrite` does: replace the file instead of writing through it,
  // which could follow a link left by an earlier build.
  match std::fs::remove_file(&path) {
    Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
    _ => {}
  }
  let mut file = std::fs::File::create_new(&path).with_context(|| format!("cannot create {}", path.display()))?;
  write_sparse(entry, &mut file).with_context(|| format!("cannot write {}", path.display()))?;
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt as _;
    file.set_permissions(std::fs::Permissions::from_mode(entry.header().mode()? & 0o7777))?;
  }
  Ok(())
}

/// Copies `reader` into `file`, seeking over blocks that hold only zeros so
/// the file system leaves them unallocated, and returns the length written.
///
/// NOTICE: this is how `cp --sparse=always` and GNU tar's `--sparse` make
/// holes. No maintained crate writes a stream sparsely; hole punching after
/// the write (`fallocate(FALLOC_FL_PUNCH_HOLE)` on Linux, `F_PUNCHHOLE` on
/// macOS) needs a code path per platform and writes the zeros first. 4 KiB is
/// the block size of ext4, APFS and most file systems; a smaller block would
/// still be stored in a whole one. The file system decides what a skipped
/// range becomes: APFS stores short ones (up to about 16 MiB) as zeros, and
/// NTFS, without `FSCTL_SET_SPARSE`, all of them; the content is the same.
fn write_sparse(reader: &mut impl std::io::Read, file: &mut std::fs::File) -> std::io::Result<u64> {
  use std::io::{Seek as _, SeekFrom, Write as _};

  const BLOCK: usize = 4096;
  let mut buffer = vec![0u8; 256 * BLOCK];
  let mut written = 0u64;
  loop {
    // Fill the buffer so blocks stay aligned to the file's offsets across
    // short reads.
    let mut filled = 0;
    while filled < buffer.len() {
      match reader.read(&mut buffer[filled..]) {
        Ok(0) => break,
        Ok(read) => filled += read,
        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
        Err(error) => return Err(error),
      }
    }
    if filled == 0 {
      break;
    }
    for block in buffer[..filled].chunks(BLOCK) {
      if block.iter().all(|&byte| byte == 0) {
        file.seek(SeekFrom::Current(block.len() as i64))?;
      } else {
        file.write_all(block)?;
      }
    }
    written += filled as u64;
  }
  // A file that ends in a hole would otherwise end at its last data block.
  file.set_len(written)?;
  Ok(written)
}

/// Runs one build and prints each step as it starts; a failed step also
/// prints the end of its output.
async fn build(docker: &Docker, context: &[u8], options: BuildImageOptions) -> Result<()> {
  let target = options.target.clone();
  let body = bollard::body_full(context.to_vec().into());
  let mut stream = docker.build_image(options, None, Some(body));
  let mut names: HashMap<String, String> = HashMap::new();
  let mut logs: HashMap<String, VecDeque<String>> = HashMap::new();
  while let Some(info) = stream.next().await {
    let info = info.with_context(|| format!("docker build of {target}"))?;
    let BuildInfo {
      aux: Some(BuildInfoAux::BuildKit(status)),
      ..
    } = info
    else {
      continue;
    };
    for vertex in status.vertexes {
      if vertex.started.is_some() && !names.contains_key(&vertex.digest) {
        output::message(format_args!("build {target}: {}", vertex.name));
        names.insert(vertex.digest.clone(), vertex.name.clone());
      }
      if !vertex.error.is_empty() {
        let output: Vec<String> = logs.remove(&vertex.digest).unwrap_or_default().into();
        bail!("docker build of {target}: {}: {}\n{}", vertex.name, vertex.error, output.join("\n"));
      }
    }
    for log in status.logs {
      let lines = logs.entry(log.vertex).or_default();
      for line in String::from_utf8_lossy(&log.msg).lines() {
        tracing::debug!(target: "needy_cli::build", "{line}");
        lines.push_back(line.to_string());
        if lines.len() > FAILED_STEP_LINES {
          lines.pop_front();
        }
      }
    }
  }
  Ok(())
}

#[cfg(all(test, unix))]
#[path = "build_test.rs"]
mod tests;
