//! `needy build`: builds actor images and the executor image with Docker.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use needy_config::recipe::{HostInfo, Recipe};

fn repo_root() -> Result<PathBuf> {
    if let Ok(root) = std::env::var("NEEDY_REPO") {
        return Ok(PathBuf::from(root));
    }
    let mut dir = std::env::current_dir()?;
    loop {
        if dir.join("images/Dockerfile").exists() {
            return Ok(dir);
        }
        if !dir.pop() {
            bail!("cannot find images/Dockerfile; set NEEDY_REPO to the needy checkout");
        }
    }
}

/// Builds the actor image for `recipe` into `out`, and the executor image.
pub fn build(recipe: Option<&Path>, out: &Path, executor: bool) -> Result<()> {
    let root = repo_root()?;
    let mut args: Vec<String> = vec![
        "build".into(),
        "-f".into(),
        root.join("images/Dockerfile").display().to_string(),
        "--target".into(),
        "actor".into(),
        "--output".into(),
        format!("type=local,dest={}", out.display()),
    ];
    if let Some(path) = recipe {
        let compiled = Recipe::load(path)?.compile(&HostInfo::detect()?, 0);
        for finding in &compiled.findings {
            eprintln!("needy: recipe {:?}: {}", finding.severity, finding.message);
        }
        if compiled
            .findings
            .iter()
            .any(|f| format!("{:?}", f.severity) == "Error")
        {
            bail!("the recipe has errors");
        }
        for (key, value) in &compiled.kernel_build_env {
            args.push("--build-arg".into());
            args.push(format!("{key}={value}"));
        }
    }
    args.push(root.display().to_string());
    std::fs::create_dir_all(out)?;
    let status = std::process::Command::new("docker")
        .args(&args)
        .status()
        .context("cannot run docker")?;
    if !status.success() {
        bail!("docker build failed");
    }
    if executor {
        let status = std::process::Command::new("docker")
            .args([
                "build",
                "-f",
                &root.join("images/Dockerfile").display().to_string(),
                "--target",
                "executor",
                "-t",
                "needy-executor:latest",
            ])
            .arg(&root)
            .status()?;
        if !status.success() {
            bail!("docker build of the executor image failed");
        }
    }
    Ok(())
}
