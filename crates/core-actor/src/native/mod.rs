//! A `macos-native` actor's own processes (a proof of concept): its init,
//! the start gate of a driving program on a terminal, and the shims.
//!
//! - [`init`] runs next to the daemon, outside the fence: it sets the actor
//!   up, starts the driving program under `sandbox-exec`, serves the
//!   kept-command service ([`kept`]) and turns the kernel's denials into
//!   trace events ([`denials`]).
//! - [`shim`] runs inside the fence, as every program on the driving
//!   program's `PATH`: it routes a routed command as a container's shim
//!   does, and has init start any other.
//!
//! See [`crate::host::macos`] and
//! `docs/ai/references/architecture/2026-11-12-macos-native-actor-poc.md`.

mod denials;
mod init;
mod kept;
mod shim;

use std::path::PathBuf;

use crate::host::macos::{GATE, INIT, Layout};

/// Becomes the actor's init, a start gate or a shim when this process was
/// started as one, and never returns then.
pub fn dispatch() {
  let mut args = std::env::args_os().skip(1);
  match args.next() {
    Some(arg) if arg == INIT => {
      let dir = args.next().map(PathBuf::from).unwrap_or_default();
      init::main(Layout::new(&dir));
    }
    Some(arg) if arg == GATE => init::gate(args.map(|a| a.to_string_lossy().into_owned()).collect()),
    _ => {}
  }
  if let Some((exe, layout)) = std::env::current_exe().ok().and_then(|exe| Layout::of_shim(&exe).map(|layout| (exe, layout))) {
    shim::main(&exe, layout);
  }
}

fn runtime() -> tokio::runtime::Runtime {
  // NOTICE: without a runtime neither init nor a shim can do anything;
  // building one fails only when the system is out of threads or memory.
  tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio runtime")
}

/// The route list shims read: [`Layout::route`]'s user line is ignored,
/// since a native actor has one user.
// NOTICE: the format and the match are `guest::shim::Route`'s, which is
// Linux-only with the rest of the guest.
// TODO(macos-native): share one route type with `guest::shim` if the
// runtime is productized.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Route {
  names: Vec<String>,
}

impl Route {
  fn parse(text: &str) -> Self {
    let names = text.lines().nth(1).unwrap_or_default().split(',').map(str::trim).filter(|n| !n.is_empty()).map(String::from).collect();
    Self { names }
  }

  fn render(&self) -> String {
    format!("0\n{}\n", self.names.join(","))
  }

  fn routes(&self, names: &[String]) -> bool {
    if self.names.len() == 1 && self.names[0] == "*" {
      return true;
    }
    names.iter().any(|name| self.names.contains(name))
  }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
