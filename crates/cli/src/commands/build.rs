//! `needy build`: builds the actor image and, with `--executor`, the executor
//! image from `images/Dockerfile`, through the Docker API with BuildKit.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use bollard::Docker;
use bollard::models::{BuildInfo, BuildInfoAux};
use bollard::query_parameters::{BuildImageOptions, BuilderVersion, ImageBuildOutput};
use futures_util::StreamExt;
use needy_core::recipe::{HostInfo, Recipe, Severity};

use crate::cli::Context;
use crate::output;

/// Lines of a failed step's output to show with its error.
const FAILED_STEP_LINES: usize = 30;

#[derive(clap::Args)]
pub struct Args {
  /// Recipe whose kernel options the actor image is built with.
  #[arg(long)]
  recipe: Option<PathBuf>,
  /// Output directory for vmlinux, rootfs.ext4 and firecracker. Defaults to
  /// the image the daemon boots.
  #[arg(long)]
  out: Option<PathBuf>,
  /// Also build the `needy-executor` Docker image.
  #[arg(long)]
  executor: bool,
}

pub async fn run(args: Args, ctx: &Context) -> Result<i32> {
  let root = repo_root()?;
  let out = match args.out {
    Some(out) => out,
    None => ctx.paths()?.default_image(),
  };
  let mut build_args = HashMap::new();
  if let Some(path) = &args.recipe {
    let compiled = Recipe::load(path).with_context(|| format!("recipe {}", path.display()))?.compile(&HostInfo::detect()?, 0);
    for finding in &compiled.findings {
      output::message(format_args!("recipe {:?}: {}", finding.severity, finding.message));
    }
    if compiled.findings.iter().any(|f| f.severity == Severity::Error) {
      bail!("the recipe has errors");
    }
    build_args.extend(compiled.kernel_build_env);
  }
  std::fs::create_dir_all(&out).with_context(|| format!("cannot create {}", out.display()))?;
  let out = std::fs::canonicalize(&out)?;
  let context = tokio::task::spawn_blocking(move || pack_context(&root)).await??;
  let docker = Docker::connect_with_defaults().context("cannot reach Docker")?;
  // NOTICE: bollard's local exporter (`ImageBuildOutput::Local`, bollard
  // 0.21.1 `src/grpc/mod.rs`, `FileSendPacketImpl`) writes files without their
  // modes, so `firecracker` would lose its executable bit. The tar exporter
  // keeps them; the tar is unpacked here and removed. Remove this when the
  // local exporter applies `Stat.mode`.
  let archive = out.join(".needy-build.tar");
  build(
    &docker,
    &context,
    BuildImageOptions {
      target: "actor".into(),
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

/// Unpacks the actor image's tar into `out`, keeping file modes.
fn unpack(archive: &Path, out: &Path) -> Result<()> {
  let file = std::fs::File::open(archive).with_context(|| format!("cannot open {}", archive.display()))?;
  let mut tar = tar::Archive::new(std::io::BufReader::new(file));
  tar.set_preserve_permissions(true);
  tar.set_overwrite(true);
  tar.unpack(out).with_context(|| format!("cannot unpack the image into {}", out.display()))
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
