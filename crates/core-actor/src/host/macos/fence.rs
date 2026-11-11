//! The fence of a `macos-native` actor: one Seatbelt profile (SBPL) per
//! run, which `sandbox-exec` applies to the driving program and to every
//! command the actor keeps.
//!
//! The profile starts open (`(allow default)`) and then denies what would
//! go around needy: every exec except the shims, the shim binary and the one
//! program a `sandbox-exec` starts (the `KEPT` parameter); every network
//! connection except the gateway's loopback port and the actor's own Unix
//! sockets; every write outside the actor's writable directories; and reads
//! of the user's secrets. Each deny carries the run's tag, so its kernel log
//! message can be found and put in the trace.
//!
//! A later rule overrides an earlier one for the same operation, which is
//! how the allows after each deny work.
//!
//! Source: the base shape follows Anthropic's sandbox-runtime, which also
//! tags denials and allows loopback proxy ports and Unix sockets by path:
//! https://github.com/anthropic-experimental/sandbox-runtime/blob/41501a60b27deb89484b718acb76a04e2a37aa75/src/sandbox/macos-sandbox-utils.ts#L1080-L1090
//! and #L1288-L1341 of the same file.
//! Reference: Apple ships no SBPL reference; the operation names are those
//! the kernel logs (`deny(1) process-exec* /bin/ls`), verified on macOS 27.2
//! (see `docs/ai/references/architecture/2026-11-12-macos-native-actor-poc.md`).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// The `sandbox-exec` parameters that name the programs a kept command or
/// the driving program may run beyond the shims: the program, and what the
/// kernel or the program executes before it runs (see [`allowed`]).
pub const KEPT: [&str; 3] = ["KEPT", "KEPT_2", "KEPT_3"];
/// A directory whose programs the kept one may run (see [`tools`]).
pub const TOOLS: &str = "TOOLS";

/// The value of a path parameter that allows nothing: an empty directory
/// every Mac has.
// NOTICE: `sandbox-exec` fails on a profile that names a parameter it was
// not given, so every parameter always gets a value.
pub const NOTHING: &str = "/var/empty";

/// What one run's fence allows beyond the shims.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fence {
  /// The tag on every deny, which finds its log message: `needy-<run>`.
  pub tag: String,
  /// The directory of shims, every program on the actor's `PATH`.
  pub shims: PathBuf,
  /// The program the shims are links to (the needy binary), by its real
  /// path: Seatbelt checks the file an exec resolves to.
  pub shim: PathBuf,
  /// The gateway's port on the loopback interface.
  pub gateway_port: u16,
  /// The Unix sockets the actor may connect to: the daemon's guest API and
  /// the kept-command service.
  pub sockets: Vec<PathBuf>,
  /// Where the actor may write: its home, its temporary directory and its
  /// workspace.
  pub writable: Vec<PathBuf>,
  /// What the actor may not read: the user's keys and credentials.
  pub hidden: Vec<PathBuf>,
}

/// Device files every program writes to: the terminal, `/dev/null`, and
/// dyld's DTrace helper (each process writes it at start; denying it only
/// fills the log).
const DEVICES: &[&str] = &["/dev/null", "/dev/zero", "/dev/tty", "/dev/dtracehelper"];

impl Fence {
  /// The profile, in SBPL.
  pub fn render(&self) -> String {
    let tag = quote(&self.tag);
    let mut p = String::new();
    // `writeln!` on a `String` cannot fail.
    let mut line = |text: String| {
      let _ = writeln!(p, "{text}");
    };
    line("(version 1)".into());
    line(format!("; needy macos-native fence, {}. Generated; do not edit.", self.tag));
    line("(allow default)".into());

    line("; Exec: the shims, the shim binary, and the one program this sandbox-exec starts.".into());
    line(format!("(deny process-exec* (with message {tag}))"));
    let kept: Vec<String> = KEPT.iter().map(|k| format!("(literal (param {}))", quote(k))).collect();
    line(format!(
      "(allow process-exec* (subpath {}) (literal {}) {} (subpath (param {})))",
      path(&self.shims),
      path(&self.shim),
      kept.join(" "),
      quote(TOOLS),
    ));

    line("; Programs that start programs outside the sandbox: Launch Services, Apple events, launchd jobs.".into());
    line(format!("(deny lsopen (with message {tag}))"));
    line(format!("(deny appleevent-send (with message {tag}))"));
    line(format!("(deny job-creation (with message {tag}))"));
    line(format!("(deny mach-lookup (with message {tag}) (global-name \"com.apple.coreservices.launchservicesd\"))"));

    line("; Network: the gateway's loopback port and the actor's own sockets.".into());
    line(format!("(deny network* (with message {tag}))"));
    line(format!("(allow network-outbound (remote ip \"localhost:{}\"))", self.gateway_port));
    // NOTICE: one rule per socket: `remote unix-socket` with two
    // `path-literal`s allowed only the first (macOS 27.2: the kept-command
    // socket, second, was denied).
    for socket in &self.sockets {
      line(format!("(allow network-outbound (remote unix-socket (path-literal {})))", path(socket)));
    }

    line("; Writes: the actor's directories, the terminal and the null devices.".into());
    let mut writable: Vec<String> = self.writable.iter().map(|w| format!("(subpath {})", path(w))).collect();
    writable.extend(DEVICES.iter().map(|d| format!("(literal {})", quote(d))));
    // A pseudo-terminal's device, which the driving program on a terminal
    // and its kept commands write.
    writable.push("(regex #\"^/dev/ttys[0-9]+$\")".into());
    // NOTICE: the `/usr/bin` trampolines of the developer tools (`python3`,
    // `git`) cache their lookup in the per-user temporary directory
    // (`confstr(_CS_DARWIN_USER_TEMP_DIR)`, not `TMPDIR`) and print
    // "couldn't create cache file …/T/xcrun_db-XXXXXXXX" on every run when
    // they cannot; seen on macOS 27.2. Only those files may be written.
    writable.push("(regex #\"^/private/var/folders/[^/]+/[^/]+/T/xcrun_db\")".into());
    line(format!("(deny file-write* (with message {tag}) (require-not (require-any {})))", writable.join(" ")));

    if !self.hidden.is_empty() {
      line("; Reads: the user's keys and credentials.".into());
      let hidden: Vec<String> = self.hidden.iter().map(|h| format!("(subpath {})", path(h))).collect();
      line(format!("(deny file-read* (with message {tag}) {})", hidden.join(" ")));
    }
    p
  }
}

/// The `sandbox-exec` arguments that start the first of `allowed` under the
/// fence in `profile`: `-f PROFILE -D KEPT=… -D KEPT_2=… -D KEPT_3=… -D
/// TOOLS=…`, then the program. `allowed` is [`allowed`]'s list; `tools` is
/// the developer directory when the program is one of its trampolines (see
/// [`tools`]).
pub fn arguments(profile: &Path, allowed: &[PathBuf], tools: Option<&Path>) -> Vec<String> {
  let program = allowed.first().map(|p| p.display().to_string()).unwrap_or_default();
  let mut args = vec!["-f".into(), profile.display().to_string()];
  for (i, name) in KEPT.iter().enumerate() {
    // An unused parameter repeats the program, which is allowed anyway.
    let value = allowed.get(i).map(|p| p.display().to_string()).unwrap_or_else(|| program.clone());
    args.extend(["-D".into(), format!("{name}={value}")]);
  }
  let tools = tools.map(|t| t.display().to_string()).unwrap_or_else(|| NOTHING.into());
  args.extend(["-D".into(), format!("{TOOLS}={tools}"), program]);
  args
}

/// What the kernel executes to start `program`, `program` first: a script's
/// interpreter from its `#!` line (Seatbelt checks both the script and its
/// interpreter), and what `/bin/sh` executes itself. At most [`KEPT`]'s
/// length; reading the file is the only IO.
// NOTICE: macOS's `/bin/sh` is a stub that re-executes `/bin/bash` ("Failed
// to exec /bin/bash as variant for /bin/sh" when that is denied), seen on
// macOS 27.2 in `.tmp/research/macos-actor/exp/sb`. A script whose `#!` line
// is `/usr/bin/env NAME` needs only `env`: it looks NAME up on the shims'
// `PATH`, so NAME is a kept command of its own.
pub fn allowed(program: &Path) -> Vec<PathBuf> {
  let mut allowed = vec![program.to_path_buf()];
  if let Some(interpreter) = interpreter(program) {
    allowed.push(interpreter);
  }
  if allowed.iter().any(|p| p == Path::new("/bin/sh")) {
    allowed.push(PathBuf::from("/bin/bash"));
  }
  let mut seen = std::collections::HashSet::new();
  allowed.retain(|p| seen.insert(p.clone()));
  allowed.truncate(KEPT.len());
  allowed
}

/// The interpreter a script's `#!` line names.
fn interpreter(program: &Path) -> Option<PathBuf> {
  use std::io::Read;
  // NOTICE: 256 bytes covers a `#!` line; the kernel reads at most 512.
  let mut head = [0u8; 256];
  let read = std::fs::File::open(program).and_then(|mut f| f.read(&mut head)).ok()?;
  let line = head[..read].strip_prefix(b"#!")?;
  let line = line.split(|b| *b == b'\n').next()?;
  let text = std::str::from_utf8(line).ok()?;
  text.split_whitespace().next().map(PathBuf::from)
}

/// The developer directory whose programs `program` runs, when `program` is
/// one of the `/usr/bin` trampolines that run the active developer tools
/// (`/usr/bin/python3`, `git`, `clang`): `developer` (what `xcode-select -p`
/// prints) when it has a program of the same name in `usr/bin`.
// NOTICE: `/usr/bin/python3` executes
// `<developer>/usr/bin/python3`, which executes the Python framework's
// `python3.9` in `<developer>/Library/Frameworks`, so the whole developer
// directory is allowed for such a command; seen on macOS 27.2.
pub fn tools(program: &Path, developer: Option<&Path>) -> Option<PathBuf> {
  let developer = developer?;
  let name = program.file_name()?;
  (program.parent() == Some(Path::new("/usr/bin")) && developer.join("usr/bin").join(name).exists()).then(|| developer.to_path_buf())
}

/// A path as an SBPL string.
fn path(p: &Path) -> String {
  quote(&p.display().to_string())
}

/// `text` as an SBPL string literal.
fn quote(text: &str) -> String {
  let mut quoted = String::with_capacity(text.len() + 2);
  quoted.push('"');
  for c in text.chars() {
    if c == '"' || c == '\\' {
      quoted.push('\\');
    }
    quoted.push(c);
  }
  quoted.push('"');
  quoted
}

#[cfg(test)]
#[path = "fence_test.rs"]
mod tests;
