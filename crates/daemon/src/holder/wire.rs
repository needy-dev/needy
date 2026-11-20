//! Private, length-delimited holder messages. Version 1 is local to this
//! binary; an incompatible holder is rejected rather than guessed from a PID.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::UnixStream;

#[derive(Serialize, Deserialize)]
pub(super) struct Request {
  pub version: u32,
  pub instance: String,
  pub token: String,
  pub command: Command,
}

#[derive(Serialize, Deserialize)]
pub(super) struct Response {
  pub instance: String,
  pub result: std::result::Result<Reply, String>,
}

#[derive(Serialize, Deserialize)]
pub(super) enum Command {
  Ping,
  Lease,
  Start(Box<Actor>),
  Inspect(String),
  EndRun(String),
  Wait(String),
  Stop { run: String, grace: Duration },
  Shutdown,
}

#[derive(Serialize, Deserialize)]
pub(super) enum Reply {
  Ready,
  Actor(Info),
  Exited(std::result::Result<i32, String>),
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Info {
  pub pid: u32,
  pub vcpus: u32,
  pub mem_mib: u32,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct Actor {
  pub run: String,
  pub dir: PathBuf,
  pub host: needy_core_actor::driver::Host,
  pub profile_yaml: String,
  pub recipe: Option<needy_core::recipe::CompiledRecipe>,
  pub mounts: Vec<needy_core_actor::driver::HostMount>,
  pub guest: PathBuf,
  #[serde(default)]
  pub layer_disks: Vec<PathBuf>,
}

// NOTICE: these local metadata messages contain a compiled recipe and mount
// list, never file archives. Bound allocation before decoding untrusted sizes.
const MAX_MESSAGE: usize = 16 * 1024 * 1024;

pub(super) async fn read<T: serde::de::DeserializeOwned>(stream: &mut UnixStream) -> Result<T> {
  let len = stream.read_u32().await? as usize;
  if len > MAX_MESSAGE {
    bail!("runtime holder message exceeds its metadata limit");
  }
  let mut bytes = vec![0; len];
  stream.read_exact(&mut bytes).await?;
  Ok(serde_json::from_slice(&bytes)?)
}

pub(super) async fn write(stream: &mut UnixStream, value: &impl Serialize) -> Result<()> {
  let bytes = serde_json::to_vec(value)?;
  if bytes.len() > MAX_MESSAGE {
    bail!("runtime holder message exceeds its metadata limit");
  }
  stream.write_u32(bytes.len() as u32).await?;
  stream.write_all(&bytes).await?;
  Ok(())
}
