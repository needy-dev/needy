//! Shims, the capture of a container actor (`linux-container`,
//! `macos-apple-container`), in two placements of the shim stub
//! (`/.needy/proxy`, `images/stub/shim.c`):
//!
//! - **Trampolines**: init hard-links a copy of the stub into
//!   [`TRAMPOLINE_DIR`], first on the driving program's `PATH`, once per
//!   program on [`SYSTEM_PATH`] ([`trampolines`]). The real program stays at
//!   its own path, so a program that finds its files from its own path, or
//!   a tool that reads a program file, behaves as without needy.
//! - **In-place shims**: the image's shim stage (`images/container/shim.sh`)
//!   copies each listed program (the shells by default) to
//!   `/.needy/real/<path>` and replaces it with a hard link to the stub, so
//!   calls by absolute path (`/bin/bash`) and `#!` interpreters are caught
//!   too. A program that is an in-place shim gets no trampoline.
//!
//! The stub hands the exec to the guest needy with the path it was started
//! through (its `/proc/self/exe`, the link), from which a shim finds the
//! program it stands for ([`real`]), and runs the real one with the caller's
//! argv, `argv[0]` included, so programs that behave by their name (busybox,
//! `python3`) see the name they were called by. Only a name on the run's
//! route list, for the route's user, goes to the proxy, which asks the
//! daemon.
//!
//! Init turns routing on by writing [`ROUTE_FILE`] after its setup, as it
//! sets the kernel module's parameters in a VM ([`super::binfmt`]); until
//! then every shim runs its real program. Init and the proxy start programs
//! through their real paths, so needy's own helpers are never routed, as
//! the kernel guarantees in a VM.
//!
//! See `docs/ai/references/architecture/2026-11-16-container-trampolines-design.md`
//! and `docs/ai/references/architecture/2026-11-10-container-actor-runtime-design.md`.

use std::ffi::{CString, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use super::Route;
use crate::contract::{NEEDY_DIR, PROXY, REAL};
use crate::guest::{Stub, proxy};

/// Turns routing on: the route's user and names, which init writes once it
/// set the actor up.
pub const ROUTE_FILE: &str = "/run/needy/route";
/// The trampolines' directory, a tmpfs, first on the driving program's
/// `PATH` in a container.
pub const TRAMPOLINE_DIR: &str = "/run/needy/shims";
/// The stub's copy in [`TRAMPOLINE_DIR`] that the trampolines are hard
/// links to: a hard link cannot cross from the root file system to the
/// tmpfs.
const TRAMPOLINE_STUB: &str = ".stub";
/// The actor's own programs, searched before the stubs. Defined with the
/// build plan, whose image `PATH` starts from it; its provenance is there.
pub use needy_core::image::SYSTEM_PATH;

/// The real program the shim at `exe` stands for, or `None` when `exe` is
/// not a shim. needy's own files in `/.needy` never are.
pub fn real(exe: &Path) -> Option<PathBuf> {
  if exe.starts_with(NEEDY_DIR) {
    return None;
  }
  if exe.parent() == Some(Path::new(TRAMPOLINE_DIR)) {
    return behind_trampoline(exe, &search_dirs(SYSTEM_PATH), Path::new(PROXY));
  }
  let real = Path::new(REAL).join(exe.strip_prefix("/").ok()?);
  (real.is_symlink() || real.exists()).then_some(real)
}

/// The program a trampoline stands for: the first program of its name in
/// `search`, at its own path, as a `PATH` lookup without the trampolines
/// finds it. An in-place shim has no trampoline, so it is never the answer.
fn behind_trampoline(exe: &Path, search: &[PathBuf], stub: &Path) -> Option<PathBuf> {
  let name = exe.file_name()?;
  search.iter().map(|dir| dir.join(name)).find(|path| is_program(path) && !is_stub(path, stub))
}

/// The directories of a `PATH` value, in order.
fn search_dirs(path: &str) -> Vec<PathBuf> {
  path.split(':').filter(|dir| !dir.is_empty()).map(PathBuf::from).collect()
}

/// Whether `path` is an executable file, through its links.
fn is_program(path: &Path) -> bool {
  use std::os::unix::fs::PermissionsExt;
  std::fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// Whether `path` is the shim stub `stub`: a hard link to it, or a copy of
/// it, as an image `FROM` the base has (see `images/container/shim.sh`'s
/// `is_proxy`).
fn is_stub(path: &Path, stub: &Path) -> bool {
  use std::os::unix::fs::MetadataExt;
  let (Ok(a), Ok(b)) = (std::fs::metadata(path), std::fs::metadata(stub)) else {
    return false;
  };
  if (a.dev(), a.ino()) == (b.dev(), b.ino()) {
    return true;
  }
  // Sizes first: reading every program on PATH would read the whole image.
  a.len() == b.len() && std::fs::read(path).ok() == std::fs::read(stub).ok()
}

/// Makes a trampoline in [`TRAMPOLINE_DIR`] for every program on
/// [`SYSTEM_PATH`] that is not an in-place shim, and returns how many.
/// Init calls it before it turns routing on, so until then a trampoline
/// runs its real program.
pub fn trampolines() -> std::io::Result<usize> {
  trampolines_in(Path::new(TRAMPOLINE_DIR), &search_dirs(SYSTEM_PATH), Path::new(PROXY))
}

/// [`trampolines`] in `dir`, for the programs in `search`, linked to a copy
/// of `stub`. The first program of a name in `search` order is the one a
/// `PATH` lookup finds; an in-place shim of that name takes its place, so
/// the name gets no trampoline and the lookup reaches the shim.
fn trampolines_in(dir: &Path, search: &[PathBuf], stub: &Path) -> std::io::Result<usize> {
  use std::os::unix::fs::PermissionsExt;
  std::fs::create_dir_all(dir)?;
  let copy = dir.join(TRAMPOLINE_STUB);
  if !copy.exists() {
    // Copied beside and renamed: an exec of a file open for writing fails
    // with `ETXTBSY`.
    let staged = dir.join(format!("{TRAMPOLINE_STUB}.new"));
    std::fs::copy(stub, &staged)?;
    std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))?;
    std::fs::rename(&staged, &copy)?;
  }
  let mut seen = std::collections::HashSet::new();
  let mut made = 0;
  let mut visited = std::collections::HashSet::new();
  for search_dir in search {
    // `/bin` is `usr/bin` on merged-/usr systems: each directory once.
    let Ok(physical) = std::fs::canonicalize(search_dir) else {
      continue;
    };
    if !visited.insert(physical) {
      continue;
    }
    let Ok(entries) = std::fs::read_dir(search_dir) else {
      continue;
    };
    let mut names: Vec<OsString> = entries.filter_map(|e| e.ok()).map(|e| e.file_name()).collect();
    names.sort();
    for name in names {
      let path = search_dir.join(&name);
      // A lookup passes over what it cannot run.
      if !is_program(&path) || !seen.insert(name.clone()) || name.as_bytes().starts_with(b".") {
        continue;
      }
      if is_stub(&path, stub) {
        continue;
      }
      let trampoline = dir.join(&name);
      if trampoline.symlink_metadata().is_ok() {
        continue;
      }
      std::fs::hard_link(&copy, &trampoline)?;
      made += 1;
    }
  }
  Ok(made)
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

/// A shim's entry, as the shim stub hands it over: runs the real program,
/// or hands a routed exec to the proxy.
pub fn main(stub: Stub) -> ! {
  let Stub { exe, execfn } = stub;
  let Some(real) = real(&exe) else {
    eprintln!("needy: {} is a shim, but its real program is missing", exe.display());
    std::process::exit(127);
  };
  let args: Vec<OsString> = std::env::args_os().collect();
  let route = std::fs::read_to_string(ROUTE_FILE).ok().and_then(|text| Route::parse(&text));
  let argv0 = args.first().map(|a| a.to_string_lossy().into_owned()).unwrap_or_default();
  let uid = nix::unistd::getuid().as_raw();
  // An in-place shim's path is the end of the caller's link chain already;
  // a trampoline's program is at the end of its real program's.
  let program = if exe.parent() == Some(Path::new(TRAMPOLINE_DIR)) {
    std::fs::canonicalize(&real).unwrap_or_else(|_| real.clone())
  } else {
    exe.clone()
  };
  let routed = route.is_some_and(|route| route.routes(uid, &names(&execfn, &program, &argv0)));
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
