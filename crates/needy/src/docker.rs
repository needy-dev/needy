use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use std::path::Path;
use std::process::Stdio;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};

#[derive(Clone)]
pub struct Docker {
    pub instance: String,
    pub image: String,
}

#[derive(Deserialize)]
pub struct Container {
    #[serde(rename = "Config")]
    pub config: ContainerConfig,
    #[serde(rename = "State")]
    pub state: ContainerState,
}
#[derive(Deserialize)]
pub struct ContainerConfig {
    #[serde(rename = "Labels")]
    pub labels: std::collections::BTreeMap<String, String>,
}
#[derive(Deserialize)]
pub struct ContainerState {
    #[serde(rename = "Status")]
    pub status: String,
    #[serde(rename = "ExitCode")]
    pub exit_code: i32,
}

pub async fn execute(args: &[String], input: Option<&[u8]>, limit: usize) -> Result<Vec<u8>> {
    let mut child = Command::new("docker")
        .args(args)
        .kill_on_drop(true)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("cannot start Docker CLI")?;
    let stdin = child.stdin.take();
    let output = child.stdout.take().unwrap();
    let errors = child.stderr.take().unwrap();
    let write = async {
        if let Some(mut stdin) = stdin {
            stdin.write_all(input.unwrap()).await?;
            stdin.shutdown().await?;
        }
        Ok::<_, anyhow::Error>(())
    };
    let read = async {
        let mut bytes = Vec::new();
        output
            .take(limit as u64 + 1)
            .read_to_end(&mut bytes)
            .await?;
        ensure!(
            bytes.len() <= limit,
            "Docker response exceeds configured limit"
        );
        Ok::<_, anyhow::Error>(bytes)
    };
    let stderr = async {
        let mut bytes = Vec::new();
        errors.take(64 << 10).read_to_end(&mut bytes).await?;
        Ok::<_, anyhow::Error>(bytes)
    };
    let (_, output, errors) = tokio::try_join!(write, read, stderr)?;
    let status = child.wait().await?;
    ensure!(
        status.success(),
        "Docker command failed: {}",
        String::from_utf8_lossy(&errors).trim()
    );
    Ok(output)
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).into()).collect()
}
fn valid_id(id: &str) -> Result<()> {
    ensure!(
        !id.is_empty()
            && id.len() <= 80
            && id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-'),
        "invalid resource ID"
    );
    Ok(())
}

impl Docker {
    pub fn prefix(&self) -> String {
        format!("needy-rs-{}", &crate::store::digest(&self.instance)[..12])
    }
    pub fn volume(&self, session: &str, kind: &str) -> String {
        format!("{}-{session}-{kind}", self.prefix())
    }
    pub fn container(&self, run: &str) -> String {
        format!("{}-{run}", self.prefix())
    }
    fn labels(&self, args: &mut Vec<String>, session: &str) {
        args.extend(strings(&[
            "--label",
            &format!("io.needy.instance={}", self.instance),
            "--label",
            &format!("io.needy.session={session}"),
        ]));
    }
    pub async fn check(&self) -> Result<()> {
        execute(
            &strings(&["info", "--format", "{{.ServerVersion}}"]),
            None,
            4096,
        )
        .await
        .map(|_| ())
    }
    pub async fn create_session(&self, session: &str) -> Result<()> {
        valid_id(session)?;
        for kind in ["workspace", "home"] {
            let mut args = strings(&["volume", "create"]);
            self.labels(&mut args, session);
            args.push(self.volume(session, kind));
            execute(&args, None, 4096).await?;
            self.require_volume(session, kind).await?;
        }
        let mut args = self.helper(session, true);
        args.extend(strings(&[
            "--mount",
            &format!(
                "type=volume,src={},dst=/home/needy",
                self.volume(session, "home")
            ),
            &self.image,
            "-c",
            "chown 1000:1000 /workspace /home/needy",
        ]));
        execute(&args, None, 65536).await?;
        Ok(())
    }
    fn helper(&self, session: &str, initialize: bool) -> Vec<String> {
        let mut args = strings(&[
            "run",
            "--rm",
            "--pull=never",
            "--network=none",
            "--read-only",
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges",
            "--memory=256m",
            "--pids-limit=32",
            "--entrypoint=/bin/sh",
            "--mount",
            &format!(
                "type=volume,src={},dst=/workspace",
                self.volume(session, "workspace")
            ),
        ]);
        if initialize {
            args.extend(strings(&["--user=0:0", "--cap-add=CHOWN"]));
        } else {
            args.push("--user=1000:1000".into());
        }
        self.labels(&mut args, session);
        args
    }
    pub async fn delete_session(&self, session: &str) -> Result<()> {
        valid_id(session)?;
        let mut owned = Vec::new();
        for kind in ["workspace", "home"] {
            let volume = self.volume(session, kind);
            let output = match execute(&strings(&["volume", "inspect", &volume]), None, 65536).await
            {
                Ok(output) => output,
                Err(_) if !self.volume_exists(&volume).await? => continue,
                Err(error) => return Err(error),
            };
            let metadata: serde_json::Value = serde_json::from_slice(&output)?;
            ensure!(
                metadata[0]["Labels"]["io.needy.instance"] == self.instance
                    && metadata[0]["Labels"]["io.needy.session"] == session,
                "refusing foreign volume"
            );
            owned.push(volume);
        }
        // Validate both owners before deleting either, and allow retry after a
        // partial deletion only when the backend confirms the volume is absent.
        for volume in owned {
            if let Err(error) = execute(&strings(&["volume", "rm", &volume]), None, 4096).await
                && self.volume_exists(&volume).await?
            {
                return Err(error);
            }
        }
        Ok(())
    }
    async fn volume_exists(&self, volume: &str) -> Result<bool> {
        let bytes = execute(
            &strings(&[
                "volume",
                "ls",
                "--format",
                "{{.Name}}",
                "--filter",
                &format!("name=^{volume}$"),
            ]),
            None,
            65536,
        )
        .await?;
        Ok(String::from_utf8(bytes)?.lines().any(|name| name == volume))
    }
    pub async fn import(&self, session: &str, bytes: &[u8]) -> Result<()> {
        self.require_volume(session, "workspace").await?;
        let mut args = self.helper(session, false);
        args.push("-i".into());
        args.extend(strings(&[
            &self.image,
            "-c",
            "tar -xf - -C /workspace --no-same-owner --no-same-permissions",
        ]));
        execute(&args, Some(bytes), 65536).await?;
        Ok(())
    }
    pub async fn export(&self, session: &str) -> Result<Vec<u8>> {
        self.require_volume(session, "workspace").await?;
        let mut args = self.helper(session, false);
        args.extend(strings(&[&self.image, "-c", "tar -cf - -C /workspace ."]));
        execute(&args, None, crate::workspace::MAX).await
    }
    pub async fn create_run(&self, session: &str, run: &str, input: &Path) -> Result<String> {
        valid_id(session)?;
        valid_id(run)?;
        for kind in ["workspace", "home"] {
            self.require_volume(session, kind).await?;
        }
        let name = self.container(run);
        let mut args = strings(&[
            "create",
            "--name",
            &name,
            "--pull=never",
            "--init",
            "--network=bridge",
            "--user=1000:1000",
            "--read-only",
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges",
            "--memory=2g",
            "--cpus=2",
            "--pids-limit=256",
            "--tmpfs=/tmp:rw,nosuid,nodev,size=256m,mode=1777",
            "--workdir=/workspace",
            "--env=HOME=/home/needy",
            "--mount",
            &format!(
                "type=volume,src={},dst=/workspace",
                self.volume(session, "workspace")
            ),
            "--mount",
            &format!(
                "type=volume,src={},dst=/home/needy",
                self.volume(session, "home")
            ),
        ]);
        self.labels(&mut args, session);
        args.extend(strings(&[
            "--log-driver=json-file",
            "--log-opt=max-size=32m",
            "--log-opt=max-file=1",
            "--mount",
            &format!("type=bind,src={},dst=/run/needy,readonly", input.display()),
            &self.image,
        ]));
        execute(&args, None, 4096).await?;
        Ok(name)
    }
    pub async fn inspect(&self, container: &str) -> Result<Container> {
        ensure!(
            container.starts_with(&format!("{}-", self.prefix())),
            "foreign resource namespace"
        );
        let mut values: Vec<Container> = serde_json::from_slice(
            &execute(&strings(&["inspect", container]), None, 65536).await?,
        )?;
        let value = values
            .pop()
            .context("container inspect returned no result")?;
        ensure!(
            value.config.labels.get("io.needy.instance") == Some(&self.instance),
            "refusing foreign container"
        );
        Ok(value)
    }

    async fn require_volume(&self, session: &str, kind: &str) -> Result<()> {
        valid_id(session)?;
        let bytes = execute(
            &strings(&["volume", "inspect", &self.volume(session, kind)]),
            None,
            65536,
        )
        .await?;
        let values: serde_json::Value = serde_json::from_slice(&bytes)?;
        ensure!(
            values[0]["Labels"]["io.needy.instance"] == self.instance
                && values[0]["Labels"]["io.needy.session"] == session,
            "refusing missing or foreign session volume"
        );
        Ok(())
    }
    pub async fn exists(&self, container: &str) -> Result<bool> {
        ensure!(
            container.starts_with(&format!("{}-", self.prefix())),
            "foreign resource namespace"
        );
        let bytes = execute(
            &strings(&[
                "container",
                "ls",
                "-a",
                "--format",
                "{{.Names}}",
                "--filter",
                &format!("name=^{container}$"),
            ]),
            None,
            65536,
        )
        .await?;
        Ok(String::from_utf8(bytes)?
            .lines()
            .any(|name| name == container))
    }
    pub async fn start(&self, container: &str) -> Result<()> {
        self.inspect(container).await?;
        execute(&strings(&["start", container]), None, 4096).await?;
        Ok(())
    }
    pub async fn stop(&self, container: &str) -> Result<()> {
        if !self.exists(container).await? {
            return Ok(());
        }
        let result = async {
            self.inspect(container).await?;
            execute(&strings(&["stop", "--time=2", container]), None, 65536).await?;
            Ok(())
        }
        .await;
        // Another cleanup may win the race; only confirmed absence is success.
        if result.is_err() && !self.exists(container).await? {
            return Ok(());
        }
        result
    }
    pub async fn remove(&self, container: &str) -> Result<()> {
        if !self.exists(container).await? {
            return Ok(());
        }
        let result = async {
            self.inspect(container).await?;
            execute(&strings(&["rm", container]), None, 65536).await?;
            Ok(())
        }
        .await;
        if result.is_err() && !self.exists(container).await? {
            return Ok(());
        }
        result
    }
    pub async fn logs(&self, container: &str) -> Result<Child> {
        self.inspect(container).await?;
        Ok(Command::new("docker")
            .args(["logs", "--follow", container])
            .kill_on_drop(true)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?)
    }
}
