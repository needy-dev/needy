//! Profiles: one session's machines, routing rules, networks, files and chaos.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Deserializer};

use crate::rules::{self, CommandContext, Decision};
pub use needy_common::filter::filter_allows;
use needy_common::util::{de_opt_duration, expand_home, glob_match};

/// The single user that the actor and every executor share.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct User {
    pub name: String,
    pub uid: u32,
    pub gid: u32,
}

impl Default for User {
    fn default() -> Self {
        Self {
            name: "agent".into(),
            uid: 1000,
            gid: 1000,
        }
    }
}

impl User {
    pub fn home(&self) -> String {
        format!("/home/{}", self.name)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Limits {
    pub single_process: bool,
    pub self_replace: bool,
}

/// A header that a gateway adds, with its value read from an environment variable.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Injection {
    pub header: String,
    pub secret: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct NetworkPolicy {
    /// Host globs that may be reached.
    pub allow: Vec<String>,
    pub inject: BTreeMap<String, Injection>,
    pub record: bool,
    /// Host to upstream URL, for plain-HTTP endpoints the gateway forwards.
    pub upstream: BTreeMap<String, String>,
}

impl NetworkPolicy {
    pub fn allows(&self, host: &str) -> bool {
        self.allow.iter().any(|pattern| glob_match(pattern, host))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ActorConfig {
    pub recipe: Option<String>,
    pub limits: Limits,
    pub network: NetworkPolicy,
    #[serde(deserialize_with = "de_words")]
    pub command: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Runtime {
    Native,
    Docker,
    Containerd,
    Firecracker,
    K8s,
    Remote,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct DockerBuild {
    pub context: String,
    pub dockerfile: Option<String>,
    pub target: Option<String>,
    pub args: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct DockerConfig {
    pub image: Option<String>,
    pub build: Option<DockerBuild>,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct RemoteConfig {
    pub url: String,
    pub token_env: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OnFull {
    #[default]
    Wait,
    New,
    Reject,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ExecutorConfig {
    pub runtime: Runtime,
    pub docker: Option<DockerConfig>,
    pub recipe: Option<String>,
    pub remote: Option<RemoteConfig>,
    pub workspace: Option<String>,
    /// Actor path prefix to executor path prefix.
    pub path_map: BTreeMap<String, String>,
    pub key: String,
    pub max_procs: Option<u32>,
    pub on_full: OnFull,
    pub max_instances: Option<u32>,
    #[serde(deserialize_with = "de_opt_duration")]
    pub idle_timeout: Option<Duration>,
}

impl Default for ExecutorConfig {
    fn default() -> Self {
        Self {
            runtime: Runtime::Docker,
            docker: None,
            recipe: None,
            remote: None,
            workspace: None,
            path_map: BTreeMap::new(),
            key: "{session}".into(),
            max_procs: None,
            on_full: OnFull::Wait,
            max_instances: None,
            idle_timeout: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ArgOps {
    #[serde(deserialize_with = "de_words")]
    pub prepend: Vec<String>,
    #[serde(deserialize_with = "de_words")]
    pub append: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct EnvOps {
    pub set: BTreeMap<String, String>,
    pub default: BTreeMap<String, String>,
    /// Prepended to list variables such as `PATH`, joined with `:`.
    pub prepend: BTreeMap<String, String>,
    pub append: BTreeMap<String, String>,
    pub unset: Vec<String>,
    pub clear: bool,
    pub pass: Vec<String>,
}

/// Actions of a command rule or preset.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Actions {
    /// `actor`, an executor name, or `executor` together with `executor:`.
    pub to: Option<String>,
    pub executor: Option<String>,
    pub deny: Option<String>,
    #[serde(deserialize_with = "de_opt_duration")]
    pub timeout: Option<Duration>,
    pub cwd: Option<String>,
    pub args: ArgOps,
    #[serde(deserialize_with = "de_words")]
    pub wrap: Vec<String>,
    #[serde(deserialize_with = "de_opt_words")]
    pub exec: Option<Vec<String>>,
    pub env: EnvOps,
    pub files: Vec<String>,
    pub faults: Vec<Fault>,
    #[serde(rename = "use")]
    pub uses: Vec<String>,
    #[serde(rename = "final")]
    pub stop: bool,
}

/// Conditions that are not positional.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct When {
    pub has: Vec<String>,
    pub cwd: Option<String>,
    pub env: BTreeMap<String, String>,
    pub under: Option<String>,
    pub tty: Option<bool>,
    /// Globs on captured arguments, keyed by capture name.
    #[serde(flatten)]
    pub captures: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Rule {
    #[serde(rename = "match")]
    pub matcher: String,
    #[serde(default)]
    pub when: When,
    #[serde(flatten)]
    pub actions: Actions,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Fault {
    /// `file.open`, `net.connect`, `exec` or `hwmon`.
    pub on: String,
    #[serde(rename = "match")]
    pub matcher: Option<String>,
    pub errno: Option<String>,
    #[serde(deserialize_with = "de_opt_duration")]
    pub delay: Option<Duration>,
    pub probability: Option<f64>,
    pub nth: Option<u64>,
    pub set: BTreeMap<String, serde_yaml::Value>,
    #[serde(deserialize_with = "de_opt_duration")]
    pub at: Option<Duration>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Chaos {
    pub seed: u64,
    pub faults: Vec<Fault>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ResolverRule {
    #[serde(rename = "match")]
    pub matcher: String,
    pub effective: Option<String>,
    pub identity: BTreeMap<String, String>,
    pub sticky: bool,
}

/// Placement for launchers whose effective command cannot be known statically.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum UnknownPolicy {
    #[default]
    Sticky,
    Actor,
    Executor(String),
}

impl<'de> Deserialize<'de> for UnknownPolicy {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        Ok(match text.as_str() {
            "sticky" => Self::Sticky,
            "actor" => Self::Actor,
            _ => Self::Executor(text),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FileMode {
    CopyIn,
    Mount,
    MirrorOut,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MirrorWhen {
    Continuous,
    #[default]
    End,
}

/// One side of a file mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Side {
    Host(String),
    Actor(String),
    Executor { name: String, path: String },
}

impl Side {
    /// Parses `host:./x`, `actor:~/x` or `executor:NAME:~/x`. `~` expands to
    /// the profile user's home on the actor and executor sides.
    pub fn parse(text: &str, user: &User) -> Result<Self> {
        if let Some(path) = text.strip_prefix("host:") {
            return Ok(Self::Host(path.to_string()));
        }
        if let Some(path) = text.strip_prefix("actor:") {
            return Ok(Self::Actor(expand_home(path, &user.name)));
        }
        if let Some(rest) = text.strip_prefix("executor:") {
            let (name, path) = rest
                .split_once(':')
                .with_context(|| format!("executor side needs NAME:PATH: {text:?}"))?;
            if name.is_empty() {
                bail!("executor side needs a name: {text:?}");
            }
            return Ok(Self::Executor {
                name: name.to_string(),
                path: expand_home(path, &user.name),
            });
        }
        bail!("file side must start with host:, actor: or executor:: {text:?}")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMapping {
    pub mode: FileMode,
    pub from: Side,
    pub to: Side,
    pub filter: Vec<String>,
    pub writable: bool,
    pub when: MirrorWhen,
    pub delete: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct RawFileMapping {
    mode: FileMode,
    from: String,
    to: String,
    #[serde(default)]
    filter: Vec<String>,
    #[serde(default)]
    writable: bool,
    #[serde(default)]
    when: MirrorWhen,
    #[serde(default)]
    delete: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct RawProfile {
    extends: Option<String>,
    user: Option<User>,
    actor: Option<ActorConfig>,
    executors: BTreeMap<String, ExecutorConfig>,
    executor_network: Option<NetworkPolicy>,
    presets: BTreeMap<String, Actions>,
    commands: Vec<Rule>,
    files: BTreeMap<String, serde_yaml::Value>,
    session_files: Option<Vec<String>>,
    chaos: Option<Chaos>,
    resolvers: Vec<ResolverRule>,
    unknown: Option<UnknownPolicy>,
    default_executor: Option<String>,
}

impl RawProfile {
    /// Child settings win; parent rules and resolvers follow the child's.
    fn merge(self, parent: RawProfile) -> RawProfile {
        let mut executors = parent.executors;
        executors.extend(self.executors);
        let mut presets = parent.presets;
        presets.extend(self.presets);
        let mut files = parent.files;
        files.extend(self.files);
        let mut commands = self.commands;
        commands.extend(parent.commands);
        let mut resolvers = self.resolvers;
        resolvers.extend(parent.resolvers);
        RawProfile {
            extends: None,
            user: self.user.or(parent.user),
            actor: self.actor.or(parent.actor),
            executors,
            executor_network: self.executor_network.or(parent.executor_network),
            presets,
            commands,
            files,
            session_files: self.session_files.or(parent.session_files),
            chaos: self.chaos.or(parent.chaos),
            resolvers,
            unknown: self.unknown.or(parent.unknown),
            default_executor: self.default_executor.or(parent.default_executor),
        }
    }
}

/// One session: machines, routing rules, networks, files and chaos.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Profile {
    pub user: User,
    pub actor: ActorConfig,
    pub executors: BTreeMap<String, ExecutorConfig>,
    pub executor_network: NetworkPolicy,
    pub presets: BTreeMap<String, Actions>,
    pub commands: Vec<Rule>,
    pub files: BTreeMap<String, FileMapping>,
    pub session_files: Vec<String>,
    pub chaos: Chaos,
    pub resolvers: Vec<ResolverRule>,
    pub unknown: UnknownPolicy,
    pub default_executor: Option<String>,
    /// Optional gitignore-style file whose lines become `-` filter rules.
    pub ignore_file: Option<String>,
    /// Directory that relative paths in the profile resolve against.
    pub base_dir: PathBuf,
}

const MAX_EXTENDS: usize = 16;

impl Profile {
    pub fn load(path: &Path) -> Result<Profile> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read profile {}", path.display()))?;
        let base = path.parent().unwrap_or(Path::new("."));
        Self::from_yaml(&text, base).with_context(|| format!("in profile {}", path.display()))
    }

    pub fn from_yaml(text: &str, base_dir: &Path) -> Result<Profile> {
        let raw = load_raw(text, base_dir, 0)?;
        Self::finish(raw, base_dir)
    }

    fn finish(raw: RawProfile, base_dir: &Path) -> Result<Profile> {
        let user = raw.user.unwrap_or_default();
        let mut files = BTreeMap::new();
        let mut ignore_file = None;
        for (name, value) in raw.files {
            if name == "ignore_file" {
                ignore_file = Some(
                    value
                        .as_str()
                        .context("files.ignore_file must be a string")?
                        .to_string(),
                );
                continue;
            }
            let mapping: RawFileMapping = serde_yaml::from_value(value)
                .with_context(|| format!("invalid file mapping {name:?}"))?;
            files.insert(
                name.clone(),
                FileMapping {
                    mode: mapping.mode,
                    from: Side::parse(&mapping.from, &user)
                        .with_context(|| format!("in file mapping {name:?}"))?,
                    to: Side::parse(&mapping.to, &user)
                        .with_context(|| format!("in file mapping {name:?}"))?,
                    filter: mapping.filter,
                    writable: mapping.writable,
                    when: mapping.when,
                    delete: mapping.delete,
                },
            );
        }
        for rule in &raw.commands {
            for preset in &rule.actions.uses {
                if !raw.presets.contains_key(preset) {
                    bail!("rule {:?} uses unknown preset {preset:?}", rule.matcher);
                }
            }
        }
        Ok(Profile {
            user,
            actor: raw.actor.unwrap_or_default(),
            executors: raw.executors,
            executor_network: raw.executor_network.unwrap_or_default(),
            presets: raw.presets,
            commands: raw.commands,
            files,
            session_files: raw.session_files.unwrap_or_default(),
            chaos: raw.chaos.unwrap_or_default(),
            resolvers: raw.resolvers,
            unknown: raw.unknown.unwrap_or_default(),
            default_executor: raw.default_executor,
            ignore_file,
            base_dir: base_dir.to_path_buf(),
        })
    }

    /// Decides where a command runs and how it is rewritten.
    pub fn decide(&self, ctx: &CommandContext) -> Decision {
        rules::decide(self, ctx)
    }

    /// Executable names the actor's kernel should hand to the proxy. `None`
    /// means every exec must be asked about.
    pub fn route_names(&self) -> Option<Vec<String>> {
        rules::route_names(self)
    }
}

fn load_raw(text: &str, base_dir: &Path, depth: usize) -> Result<RawProfile> {
    if depth > MAX_EXTENDS {
        bail!("profile extends nests more than {MAX_EXTENDS} levels");
    }
    let raw: RawProfile = if text.trim().is_empty() {
        RawProfile::default()
    } else {
        serde_yaml::from_str(text).context("invalid profile YAML")?
    };
    let Some(parent) = raw.extends.clone() else {
        return Ok(raw);
    };
    let path = base_dir.join(&parent);
    let parent_text = std::fs::read_to_string(&path)
        .with_context(|| format!("cannot read parent profile {}", path.display()))?;
    let parent_base = path.parent().unwrap_or(base_dir).to_path_buf();
    let parent = load_raw(&parent_text, &parent_base, depth + 1)
        .with_context(|| format!("in parent profile {}", path.display()))?;
    Ok(raw.merge(parent))
}

const DAEMON_OWNED: &[&str] = &["--network", "--net", "--name", "--label", "-l"];
const VALUE_FLAGS: &[&str] = &[
    "--network",
    "--net",
    "--name",
    "--label",
    "-l",
    "--cap-add",
    "--device",
    "--pid",
    "--ipc",
    "--userns",
    "--uts",
    "--security-opt",
    "-v",
    "--volume",
    "--mount",
];

/// Checks docker run options from a profile. Fields the daemon owns are always
/// refused; privileged options pass only when `operator_allow` names them
/// (`--privileged`, `--cap-add`, `--device`, `--pid`, `--ipc`, `--userns`,
/// `--uts`, `--security-opt`, `host-root`).
pub fn check_docker_args(args: &[String], operator_allow: &[String]) -> Result<(), String> {
    let allowed = |name: &str| operator_allow.iter().any(|a| a == name);
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with('-') => (f.to_string(), Some(v.to_string())),
            _ => (arg.clone(), None),
        };
        let value = if inline.is_some() {
            inline
        } else if VALUE_FLAGS.contains(&flag.as_str()) {
            i += 1;
            args.get(i).cloned()
        } else {
            None
        };
        i += 1;
        if DAEMON_OWNED.contains(&flag.as_str()) {
            return Err(format!("{flag} is set by the daemon"));
        }
        let value = value.unwrap_or_default();
        match flag.as_str() {
            "--privileged" | "--cap-add" | "--device" if !allowed(&flag) => {
                return Err(format!("{flag} is not allowed by the operator"));
            }
            "--pid" | "--ipc" | "--userns" | "--uts" if value == "host" && !allowed(&flag) => {
                return Err(format!("{flag} host is not allowed by the operator"));
            }
            "--security-opt" if value.contains("unconfined") && !allowed(&flag) => {
                return Err(format!("{flag} {value} is not allowed by the operator"));
            }
            "-v" | "--volume" => {
                let mut parts = value.split(':');
                let source = parts.next().unwrap_or_default();
                let target = parts.next().unwrap_or_default();
                check_mount(source, target, &allowed)?;
            }
            "--mount" => {
                let mut source = "";
                let mut target = "";
                for pair in value.split(',') {
                    match pair.split_once('=') {
                        Some(("source" | "src", v)) => source = v,
                        Some(("target" | "dst" | "destination", v)) => target = v,
                        _ => {}
                    }
                }
                check_mount(source, target, &allowed)?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn check_mount(source: &str, target: &str, allowed: &dyn Fn(&str) -> bool) -> Result<(), String> {
    if target.split('/').any(|part| part == "workspace") {
        return Err(format!(
            "the workspace mount at {target} is set by the daemon"
        ));
    }
    if source == "/" && !allowed("host-root") {
        return Err("mounting the host root is not allowed by the operator".into());
    }
    Ok(())
}

fn de_words<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    Ok(de_opt_words(d)?.unwrap_or_default())
}

/// A word list written as a YAML sequence or as one space-separated string.
fn de_opt_words<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Vec<String>>, D::Error> {
    let value = Option::<serde_yaml::Value>::deserialize(d)?;
    match value {
        None | Some(serde_yaml::Value::Null) => Ok(None),
        Some(serde_yaml::Value::String(s)) => {
            Ok(Some(s.split_whitespace().map(str::to_string).collect()))
        }
        Some(serde_yaml::Value::Sequence(items)) => items
            .into_iter()
            .map(|item| match item {
                serde_yaml::Value::String(s) => Ok(s),
                serde_yaml::Value::Number(n) => Ok(n.to_string()),
                serde_yaml::Value::Bool(b) => Ok(b.to_string()),
                _ => Err(serde::de::Error::custom("expected a word")),
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some),
        Some(_) => Err(serde::de::Error::custom("expected a string or a list")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROFILE: &str = r#"
user: { name: agent, uid: 1000, gid: 1000 }
actor:
  recipe: gce-e2-standard-4
  limits: { single_process: true, self_replace: true }
  command: opencode run hello
  network:
    allow: [openrouter.ai, "*.models.dev"]
    inject:
      openrouter.ai: { header: Authorization, secret: OPENROUTER_KEY }
executors:
  build:
    runtime: docker
    docker:
      build: { context: ./env, dockerfile: Dockerfile.build, target: toolchain, args: { RUST_VERSION: "1.97" } }
      args: [--cpus=4, --memory=8g]
    key: "{session}"
    max_procs: 4
    on_full: wait
    max_instances: 16
    idle_timeout: 10m
  mac:
    runtime: remote
    remote: { url: "https://mac-mini.local:7331" }
    key: "{rule}"
    max_procs: 1
executor_network:
  allow: [registry.npmjs.org]
  record: true
files:
  skills: { mode: copy-in, from: "host:./.agents/", to: "actor:~/.agents/" }
  codex-home:
    mode: copy-in
    from: "host:~/.codex/"
    to: "actor:~/.codex/"
    filter: ["- /auth.json", "- /sessions/**", "+ **"]
  workspace: { mode: mount, from: "executor:build:~/workspace/", to: "actor:~/workspace/", writable: true }
  sessions: { mode: mirror-out, from: "actor:~/.codex/sessions/", to: "host:./sessions/", when: continuous }
  ignore_file: .needyignore
session_files: [skills, workspace]
chaos:
  seed: 42
  faults:
    - { on: file.open, match: "/workspace/**/*.json", errno: EIO, probability: 0.01 }
    - { on: net.connect, match: registry.npmjs.org, delay: 2s, nth: 3 }
    - { on: hwmon, set: { temp_c: 101 }, at: 5m }
"#;

    #[test]
    fn parses_full_profile() {
        let p = Profile::from_yaml(PROFILE, Path::new("/tmp")).unwrap();
        assert_eq!(p.user.home(), "/home/agent");
        assert!(p.actor.limits.single_process);
        assert_eq!(p.actor.command, ["opencode", "run", "hello"]);
        assert!(p.actor.network.allows("api.models.dev"));
        assert!(!p.actor.network.allows("example.com"));
        let build = &p.executors["build"];
        assert_eq!(build.runtime, Runtime::Docker);
        assert_eq!(build.idle_timeout, Some(Duration::from_secs(600)));
        assert_eq!(build.max_procs, Some(4));
        assert_eq!(
            build.docker.as_ref().unwrap().args,
            ["--cpus=4", "--memory=8g"]
        );
        let mac = &p.executors["mac"];
        assert_eq!(mac.runtime, Runtime::Remote);
        assert_eq!(mac.key, "{rule}");
        assert_eq!(mac.on_full, OnFull::Wait);
        assert_eq!(
            p.files["workspace"].from,
            Side::Executor {
                name: "build".into(),
                path: "/home/agent/workspace/".into()
            }
        );
        assert_eq!(
            p.files["skills"].to,
            Side::Actor("/home/agent/.agents/".into())
        );
        assert_eq!(p.files["codex-home"].from, Side::Host("~/.codex/".into()));
        assert_eq!(p.files["sessions"].when, MirrorWhen::Continuous);
        assert_eq!(p.ignore_file.as_deref(), Some(".needyignore"));
        assert_eq!(p.chaos.seed, 42);
        assert_eq!(p.chaos.faults[1].delay, Some(Duration::from_secs(2)));
        assert_eq!(
            p.chaos.faults[0].matcher.as_deref(),
            Some("/workspace/**/*.json")
        );
        assert_eq!(p.chaos.faults[2].at, Some(Duration::from_secs(300)));
    }

    #[test]
    fn extends_merges_child_first() {
        let dir = std::env::temp_dir().join(format!("needy-config-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("base.yaml"),
            "default_executor: build\nexecutors: { build: { runtime: docker }, mac: { runtime: remote } }\ncommands:\n  - { match: \"**\", env: { set: { BASE: \"1\" } } }\n",
        )
        .unwrap();
        let child = "extends: base.yaml\nexecutors: { mac: { runtime: native } }\ncommands:\n  - { match: \"git **\", to: actor }\n";
        let p = Profile::from_yaml(child, &dir).unwrap();
        assert_eq!(p.executors["mac"].runtime, Runtime::Native);
        assert_eq!(p.executors["build"].runtime, Runtime::Docker);
        assert_eq!(p.commands[0].matcher, "git **");
        assert_eq!(p.commands[1].matcher, "**");
        assert_eq!(p.default_executor.as_deref(), Some("build"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rejects_unknown_preset_and_bad_side() {
        assert!(
            Profile::from_yaml("commands: [{ match: git, use: [nope] }]", Path::new(".")).is_err()
        );
        assert!(
            Profile::from_yaml(
                "files: { a: { mode: mount, from: \"nowhere\", to: \"actor:~/a\" } }",
                Path::new(".")
            )
            .is_err()
        );
    }

    #[test]
    fn filters_follow_rclone() {
        let f: Vec<String> = ["- /auth.json", "- /sessions/**", "- *.log", "+ **"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert!(!filter_allows(&f, "auth.json"));
        assert!(filter_allows(&f, "nested/auth.json"));
        assert!(!filter_allows(&f, "sessions/2026/a.jsonl"));
        assert!(!filter_allows(&f, "deep/dir/x.log"));
        assert!(filter_allows(&f, "config.toml"));
        let dir: Vec<String> = vec!["- node_modules/".into()];
        assert!(!filter_allows(&dir, "pkg/node_modules/x/y.js"));
        assert!(filter_allows(&[], "anything"));
    }

    #[test]
    fn docker_args_are_checked() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(check_docker_args(&a(&["--cpus=4", "--memory", "8g"]), &[]).is_ok());
        assert!(check_docker_args(&a(&["--network", "host"]), &[]).is_err());
        assert!(check_docker_args(&a(&["--name=x"]), &[]).is_err());
        assert!(check_docker_args(&a(&["--privileged"]), &[]).is_err());
        assert!(check_docker_args(&a(&["--privileged"]), &a(&["--privileged"])).is_ok());
        assert!(check_docker_args(&a(&["--pid", "host"]), &[]).is_err());
        assert!(check_docker_args(&a(&["--pid", "container:x"]), &[]).is_ok());
        assert!(check_docker_args(&a(&["-v", "/:/host"]), &[]).is_err());
        assert!(check_docker_args(&a(&["-v", "/:/host"]), &a(&["host-root"])).is_ok());
        assert!(check_docker_args(&a(&["-v", "/data:/home/agent/workspace"]), &[]).is_err());
        assert!(check_docker_args(&a(&["--mount", "type=bind,source=/,target=/h"]), &[]).is_err());
        assert!(check_docker_args(&a(&["-v", "/cache:/cache:ro"]), &[]).is_ok());
        assert!(check_docker_args(&a(&["--security-opt", "seccomp=unconfined"]), &[]).is_err());
    }
}
