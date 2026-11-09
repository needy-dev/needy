//! The `needy` binary: CLI, daemon, executor, and the actor guest supervisor and proxy.

mod cli;
mod commands;
mod logging;
mod output;
mod paths;

fn main() {
  // Inside an actor the same binary is the init process and the exec proxy.
  #[cfg(target_os = "linux")]
  guest();
  std::process::exit(cli::run());
}

/// Becomes the actor's init or proxy when started as one; actors are Linux.
#[cfg(target_os = "linux")]
fn guest() {
  use needy_core_actor::guest;
  // The kernel starts init before /proc is mounted, so check argv[0]; a
  // container's PID 1 is not an actor's init.
  if std::env::args_os().next().as_deref() == Some(std::ffi::OsStr::new("/.needy/init")) {
    guest::init_main();
  }
  if let Ok(exe) = std::env::current_exe() {
    match exe.to_str() {
      Some("/.needy/proxy") => guest::proxy_main(),
      Some("/.needy/init") => guest::init_main(),
      _ => {}
    }
  }
}
