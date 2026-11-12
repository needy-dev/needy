//! Build-time shims, the capture of a container actor (`linux-container`,
//! `macos-apple-container`).
//!
//! The container actor image's last stage (`images/container/shim.sh`)
//! copies every executable on the image's `PATH` to `/.needy/real/<path>`
//! and replaces it with a hard link to this binary. A shim finds the program
//! it stands for from `/proc/self/exe`, which names the link it was started
//! through, and runs the real one with the caller's argv, `argv[0]`
//! included, so programs that behave by their name (busybox, `python3`)
//! see the name they were called by. Only a name on the run's route list,
//! for the route's user, goes to the proxy, which asks the daemon.
//!
//! Init turns routing on by writing [`ROUTE_FILE`] after its setup, as it
//! sets the kernel module's parameters in a VM ([`super::binfmt`]); until then, and in the
//! image build, every shim runs its real program. Init and the proxy start
//! programs through their real paths, so needy's own helpers are never
//! routed, as the kernel guarantees in a VM.
//!
//! See `docs/ai/references/architecture/2026-11-10-container-actor-runtime-design.md`.

use std::ffi::{CString, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use super::Route;
use crate::contract::{NEEDY_DIR, REAL};
use crate::guest::proxy;

/// Turns routing on: the route's user and names, which init writes once it
/// set the actor up.
pub const ROUTE_FILE: &str = "/run/needy/route";

/// The real program the shim at `exe` stands for, or `None` when `exe` is
/// not a shim. needy's own files in `/.needy` never are.
pub fn real(exe: &Path) -> Option<PathBuf> {
  if exe.starts_with(NEEDY_DIR) {
    return None;
  }
  let real = Path::new(REAL).join(exe.strip_prefix("/").ok()?);
  (real.is_symlink() || real.exists()).then_some(real)
}

/// Whether this process was started through a shim.
pub fn invoked() -> bool {
  std::env::current_exe().ok().and_then(|exe| real(&exe)).is_some()
}

// The route file's format; what a route routes is the capture's (`super`).
impl Route {
  /// Reads [`ROUTE_FILE`]'s format: the user ID on the first line, the
  /// names comma-separated on the second, as the kernel module's `route`
  /// parameter has them.
  pub fn parse(text: &str) -> Option<Self> {
    let mut lines = text.lines();
    let uid = lines.next()?.trim().parse().ok()?;
    let names = lines.next().unwrap_or_default().split(',').map(str::trim).filter(|n| !n.is_empty()).map(String::from).collect();
    Some(Self { uid, names })
  }

  pub fn render(&self) -> String {
    format!("{}\n{}\n", self.uid, self.names.join(","))
  }
}

/// The names an exec is matched by: the path the caller named
/// (`AT_EXECFN`), its file name, the shim's own file name (the program a
/// link chain such as `python3` -> `python3.12` ends at), and `argv[0]` and
/// its file name, which name a script's interpreter as its `#!` line wrote
/// it. The kernel module matches the path, its file name and the file's
/// name the same way.
pub fn names(execfn: &str, exe: &Path, argv0: &str) -> Vec<String> {
  let base = |path: &str| Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned());
  let mut names = vec![execfn.to_string()];
  names.extend(base(execfn));
  names.extend(exe.file_name().map(|n| n.to_string_lossy().into_owned()));
  names.push(argv0.to_string());
  names.extend(base(argv0));
  names.retain(|n| !n.is_empty());
  names.dedup();
  names
}

/// The program `program` names on `search` (a `PATH` value), as its real
/// file when it is a shim: init starts the driving program this way, so the
/// program itself is never routed. `None` when it is not a shim or not
/// found, and the caller starts `program` as it is.
pub fn resolve(program: &str, search: &str) -> Option<PathBuf> {
  let found = if program.contains('/') {
    PathBuf::from(program)
  } else {
    search.split(':').map(|dir| Path::new(dir).join(program)).find(|path| path.exists())?
  };
  real(&std::fs::canonicalize(found).ok()?)
}

/// Turns routing on for `route`, readable by every user and writable by
/// init only.
pub fn enable(route: &Route) -> std::io::Result<()> {
  use std::os::unix::fs::PermissionsExt;
  let path = Path::new(ROUTE_FILE);
  if let Some(dir) = path.parent() {
    std::fs::create_dir_all(dir)?;
  }
  // Written beside and renamed, so a shim never reads half a file.
  let staged = path.with_extension("new");
  std::fs::write(&staged, route.render())?;
  std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o644))?;
  std::fs::rename(&staged, path)
}

/// A shim's entry: runs the real program, or hands a routed exec to the
/// proxy.
pub fn main() -> ! {
  let Some((exe, real)) = std::env::current_exe().ok().and_then(|exe| real(&exe).map(|real| (exe, real))) else {
    eprintln!("needy: this program is a shim, but its real program is missing");
    std::process::exit(127);
  };
  let args: Vec<OsString> = std::env::args_os().collect();
  let route = std::fs::read_to_string(ROUTE_FILE).ok().and_then(|text| Route::parse(&text));
  let execfn = proxy::exec_path();
  let argv0 = args.first().map(|a| a.to_string_lossy().into_owned()).unwrap_or_default();
  let uid = nix::unistd::getuid().as_raw();
  let routed = route.is_some_and(|route| route.routes(uid, &names(&execfn, &exe, &argv0)));
  if !routed {
    exec(&real, &args);
  }
  // The caller named this program, or, for a script, the kernel started
  // its interpreter, whose name the `#!` line gave as `argv[0]`.
  let direct = std::fs::canonicalize(&execfn).is_ok_and(|path| path == exe);
  let path = if direct { execfn } else { argv0 };
  proxy::run(proxy::Invocation {
    path,
    args: args.iter().map(|a| a.to_string_lossy().into_owned()).collect(),
    local: proxy::Local::Program(real.to_string_lossy().into_owned()),
  })
}

/// Executes `program` with `args` as its argv, unchanged.
fn exec(program: &Path, args: &[OsString]) -> ! {
  // NOTICE: a path or argument from `execve` cannot hold a NUL byte.
  let path = CString::new(program.as_os_str().as_bytes()).unwrap_or_default();
  let argv: Vec<CString> = args.iter().map(|a| CString::new(a.as_bytes()).unwrap_or_default()).collect();
  let Err(errno) = nix::unistd::execv(&path, &argv);
  eprintln!("needy: cannot run {}: {}", program.display(), std::io::Error::from(errno));
  std::process::exit(127)
}

#[cfg(test)]
#[path = "shim_test.rs"]
mod tests;
