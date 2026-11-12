//! The `needy` binary: CLI, daemon, executor, and the actor guest supervisor and proxy.

mod cli;
mod commands;
mod credentials;
mod logging;
mod output;
mod paths;
mod runs;

fn main() {
  // Inside an actor the same binary, injected as the guest needy, is the
  // init process, the exec proxy and the shims.
  #[cfg(target_os = "linux")]
  guest();
  std::process::exit(cli::run());
}

/// Becomes the actor's init, proxy or a shim when one of the base image's
/// stubs started it (the base ABI, `needy_core_actor::image::base`); actors
/// are Linux.
#[cfg(target_os = "linux")]
fn guest() {
  use needy_core_actor::contract::{GUEST, INIT_ARG};
  use needy_core_actor::guest;
  // The boot stub starts `/.needy/needy init` before /proc is mounted, so
  // argv says it; init started again as the user's program says so too.
  let mut args = std::env::args_os();
  if args.next().as_deref() == Some(std::ffi::OsStr::new(GUEST)) && args.next().as_deref() == Some(std::ffi::OsStr::new(INIT_ARG)) {
    guest::init_main();
  }
  // SAFETY: first thing in `main`, before any thread exists.
  if let Some(stub) = unsafe { guest::Stub::take() } {
    guest::stub_main(stub);
  }
}
