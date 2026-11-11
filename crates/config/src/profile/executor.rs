//! Executors: where routed commands run, and how their instances are pooled.

use std::collections::BTreeMap;

use std::fmt;
use std::net::IpAddr;

use serde::Deserialize;
use serde::de::{self, Deserializer, SeqAccess, Visitor};

use super::runner::RunnerSelector;
use crate::value::{ByteSize, Duration, Template};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Executor {
  /// The runner the executor runs on; the control host itself when absent.
  pub runner: Option<RunnerSelector>,
  /// What the runner creates for it: its own process space (`native`, the
  /// default) or a container (`docker`).
  pub provider: Provider,
  pub docker: Option<Docker>,
  /// A recipe reference, for providers that build a machine.
  pub recipe: Option<String>,
  /// The working directory of a command that names none: a path on the
  /// executor's machine.
  pub workspace: Option<String>,
  /// Actor path prefix to executor path prefix.
  pub path_map: BTreeMap<String, String>,
  /// Commands whose key renders equal share one instance: `{run}`,
  /// `{profile}`, `{group}`, `{rule}`.
  pub key: Template,
  /// Matched commands running at once per instance.
  pub max_procs: Option<u32>,
  pub on_full: OnFull,
  pub max_instances: Option<u32>,
  pub idle_timeout: Option<Duration>,
}

impl Default for Executor {
  fn default() -> Self {
    Self {
      runner: None,
      provider: Provider::Native,
      docker: None,
      recipe: None,
      workspace: None,
      path_map: BTreeMap::new(),
      key: Template::new("{run}"),
      max_procs: None,
      on_full: OnFull::Wait,
      max_instances: None,
      idle_timeout: None,
    }
  }
}

/// What creates an executor's instance on its runner.
// NOTICE: `remote` (another machine's `needy daemon --mode executor`) was
// removed with slice 5 of the runners-and-runs design: runners dial the
// control plane and profiles select them with `runner:`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
  /// The runner's own process space.
  Native,
  /// A container the runner's machine creates.
  Docker,
  Containerd,
  Firecracker,
  K8s,
}

/// What a command does when its instance has no free slot.
// NOTICE: `new` (start another instance with the same key) was removed
// (owner decision, 2026-11-11): it waited like `wait`, since a second
// instance per key needs `max_instances`, which is not built. A profile that
// still names it is refused by serde's message naming `wait` and `reject`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OnFull {
  #[default]
  Wait,
  Reject,
}

/// A Docker executor's image and the run options of its container, as typed
/// fields that the `docker` provider sets on the Docker API's container body
/// one by one.
///
/// Fields the daemon sets (hostname, user, entrypoint, command, network,
/// name, labels, auto-removal, the workspace and socket mounts) are not
/// here, so a profile cannot name them. `privileged`, `cap_add`, `devices`,
/// a `host` `pid`, `ipc`, `userns` or `uts` mode, an `unconfined`
/// `security_opt` and a bind mount outside the workspace are refused when
/// the executor is created unless the operator allows them.
// NOTICE: `docker.args` (`docker run` strings translated through an
// allowlist) was replaced by these fields on 2026-11-11 without
// compatibility; `refuse_removed` in `profile/mod.rs` names them when a
// profile still has it.
// TODO(docker): `docker run` options without a field (ports,
// `--device-cgroup-rule`, `--cpu-period`/`--cpu-quota`,
// `--memory-reservation`, `--oom-*`, blkio, `--runtime`, `--storage-opt`,
// `--gpus` drivers, capabilities and options, a tmpfs `mode`) are left out
// until a profile needs one; each maps to one field of the Docker API's
// `HostConfig`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Docker {
  pub image: Option<String>,
  pub build: Option<DockerBuild>,
  /// CPUs the container may use, as `docker run --cpus`: `1.5`.
  pub cpus: Option<Cpus>,
  /// The relative CPU weight (1024 is Docker's default).
  pub cpu_shares: Option<i64>,
  /// CPUs the container may run on: `0-3`, `0,1`.
  pub cpuset_cpus: Option<String>,
  pub memory: Option<ByteSize>,
  /// Memory plus swap; `unlimited` for no swap limit.
  pub memory_swap: Option<MemorySwap>,
  /// The size of `/dev/shm`.
  pub shm_size: Option<ByteSize>,
  /// Processes the container may hold at once; -1 for no limit.
  pub pids_limit: Option<i64>,
  /// Resource limits by name (`nofile`, `nproc`): one number for both the
  /// soft and the hard limit, or `{ soft, hard }`.
  pub ulimits: BTreeMap<String, Ulimit>,
  /// The container's environment, which the executor's processes inherit.
  pub env: BTreeMap<String, String>,
  /// The container's working directory, an absolute path in the container.
  pub workdir: Option<String>,
  /// Mounts the container's root file system read-only.
  pub read_only: Option<bool>,
  /// Runs an init process as PID 1 that reaps zombies.
  pub init: Option<bool>,
  /// Mounts besides the workspace and the socket the daemon mounts.
  pub mounts: Vec<Mount>,
  /// Host names to add to the container's `/etc/hosts`: name to address.
  pub add_host: BTreeMap<String, String>,
  /// DNS servers.
  pub dns: Vec<IpAddr>,
  /// Namespaced kernel parameters: `net.ipv4.ip_forward: "1"`.
  pub sysctls: BTreeMap<String, String>,
  /// GPUs for the container, through the host's GPU runtime.
  pub gpus: Option<Gpus>,
  /// Capabilities to drop.
  pub cap_drop: Vec<String>,
  /// Needs the operator.
  pub privileged: bool,
  /// Capabilities to add; needs the operator.
  pub cap_add: Vec<String>,
  /// Host devices; needs the operator.
  pub devices: Vec<Device>,
  /// The PID namespace: `host` needs the operator, `container:NAME` does not.
  pub pid: Option<String>,
  /// The IPC namespace: `host` needs the operator.
  pub ipc: Option<String>,
  /// The user namespace: `host` needs the operator.
  pub userns: Option<String>,
  /// The UTS namespace: `host` needs the operator.
  pub uts: Option<String>,
  /// Security options; an `unconfined` one needs the operator.
  pub security_opt: Vec<String>,
}

/// CPUs as `docker run --cpus` takes them, kept as billionths of a CPU (the
/// Docker API's `NanoCpus`) so that the schema stays `Eq`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cpus {
  nanos: u64,
}

impl Cpus {
  /// Billionths of a CPU.
  pub const fn nanos(&self) -> u64 {
    self.nanos
  }

  fn new(cpus: f64) -> Result<Self, String> {
    if cpus.is_finite() && cpus > 0.0 && cpus < 1e9 {
      Ok(Self {
        nanos: (cpus * 1e9).round() as u64,
      })
    } else {
      Err(format!("invalid cpus {cpus}: expected a number above 0, such as 1.5"))
    }
  }
}

impl<'de> Deserialize<'de> for Cpus {
  fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
    struct V;
    impl Visitor<'_> for V {
      type Value = Cpus;

      fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a number of CPUs above 0, such as 1.5")
      }

      fn visit_u64<E: de::Error>(self, n: u64) -> Result<Cpus, E> {
        Cpus::new(n as f64).map_err(E::custom)
      }

      fn visit_i64<E: de::Error>(self, n: i64) -> Result<Cpus, E> {
        Cpus::new(n as f64).map_err(E::custom)
      }

      fn visit_f64<E: de::Error>(self, n: f64) -> Result<Cpus, E> {
        Cpus::new(n).map_err(E::custom)
      }
    }
    d.deserialize_any(V)
  }
}

/// Memory plus swap: a size, or `unlimited` (the Docker API's -1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemorySwap {
  Limit(ByteSize),
  Unlimited,
}

impl<'de> Deserialize<'de> for MemorySwap {
  fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
    struct V;
    impl Visitor<'_> for V {
      type Value = MemorySwap;

      fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a size such as 16GiB, or unlimited")
      }

      fn visit_u64<E: de::Error>(self, mib: u64) -> Result<MemorySwap, E> {
        Ok(MemorySwap::Limit(ByteSize::from_mib(mib)))
      }

      fn visit_str<E: de::Error>(self, text: &str) -> Result<MemorySwap, E> {
        if text == "unlimited" {
          return Ok(MemorySwap::Unlimited);
        }
        text.parse().map(MemorySwap::Limit).map_err(|e| E::custom(format!("{e}, or unlimited")))
      }
    }
    d.deserialize_any(V)
  }
}

/// One resource limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(
  untagged,
  deny_unknown_fields,
  expecting = "a limit, or { soft, hard }"
)]
pub enum Ulimit {
  /// The soft and the hard limit.
  Same(i64),
  Pair {
    soft: i64,
    hard: i64,
  },
}

/// A mount besides the daemon's.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum Mount {
  /// A host directory or file, which must exist: inside the executor's
  /// workspace on the host, or anywhere the operator allows.
  Bind {
    source: String,
    target: String,
    #[serde(default)]
    read_only: bool,
  },
  /// A Docker volume; one without `source` is anonymous.
  Volume {
    #[serde(default)]
    source: Option<String>,
    target: String,
    #[serde(default)]
    read_only: bool,
  },
  /// A memory file system.
  Tmpfs {
    target: String,
    #[serde(default)]
    size: Option<ByteSize>,
  },
}

/// GPUs: `all`, a number of GPUs, or a list of device IDs (indexes or
/// UUIDs), as `docker run --gpus` takes them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gpus {
  All,
  Count(u32),
  Devices(Vec<String>),
}

impl<'de> Deserialize<'de> for Gpus {
  fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
    struct V;
    impl<'de> Visitor<'de> for V {
      type Value = Gpus;

      fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("all, a number of GPUs or a list of device IDs")
      }

      fn visit_u64<E: de::Error>(self, n: u64) -> Result<Gpus, E> {
        u32::try_from(n).map(Gpus::Count).map_err(|_| E::invalid_value(de::Unexpected::Unsigned(n), &self))
      }

      fn visit_str<E: de::Error>(self, text: &str) -> Result<Gpus, E> {
        match text {
          "all" => Ok(Gpus::All),
          other => Err(E::invalid_value(de::Unexpected::Str(other), &self)),
        }
      }

      fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Gpus, A::Error> {
        let mut ids = Vec::new();
        while let Some(id) = seq.next_element::<String>()? {
          ids.push(id);
        }
        Ok(Gpus::Devices(ids))
      }
    }
    d.deserialize_any(V)
  }
}

/// A host device in the container.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Device {
  pub host: String,
  /// The path in the container; the host's when absent.
  #[serde(default)]
  pub container: Option<String>,
  /// cgroup permissions, some of `r`, `w` and `m`; `rwm` when absent.
  #[serde(default)]
  pub permissions: Option<String>,
}

/// `docker build` options.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DockerBuild {
  pub context: String,
  pub dockerfile: Option<String>,
  pub target: Option<String>,
  /// Build arguments.
  pub args: BTreeMap<String, String>,
}
