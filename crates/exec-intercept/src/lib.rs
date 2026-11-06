//! Selective Linux exec interception with caller-owned placement policy.
//!
//! Only execve/execveat enter the tracer. Redirects allocate their own tracee
//! memory; the library never places strings in unused stack space. The root's
//! first exec is local, and redirected tasks and their descendants stay local.
//! This mechanism does not provide a security sandbox.

use std::ffi::OsString;
use std::path::PathBuf;

/// Initial process. Standard streams and file descriptors are inherited.
#[derive(Debug, Clone)]
pub struct Command {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub cwd: PathBuf,
    pub environment: Vec<OsString>,
}

impl Command {
    pub fn new(program: impl Into<PathBuf>) -> std::io::Result<Self> {
        Ok(Self {
            program: program.into(),
            args: Vec::new(),
            cwd: std::env::current_dir()?,
            environment: std::env::vars_os()
                .map(|(key, value)| {
                    let mut entry = key;
                    entry.push("=");
                    entry.push(value);
                    entry
                })
                .collect(),
        })
    }
}

/// Snapshot at an exec boundary. `argv` includes the original argv[0].
#[derive(Debug, Clone)]
pub struct Exec {
    pub pid: i32,
    pub parent_pid: Option<i32>,
    pub executable: OsString,
    pub argv: Vec<OsString>,
    pub environment: Vec<OsString>,
    pub cwd: PathBuf,
    pub dir_fd: Option<i32>,
    pub flags: u64,
}

/// Replacement argv includes argv[0]. Its executable must be absolute.
#[derive(Debug, Clone)]
pub struct Replacement {
    pub executable: PathBuf,
    pub argv: Vec<OsString>,
    /// None preserves the intercepted environment.
    pub environment: Option<Vec<OsString>>,
}

#[derive(Debug, Clone)]
pub enum Action {
    Continue,
    Redirect(Replacement),
    /// Return a positive errno to the program without executing its request.
    Deny(i32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Exited(i32),
    Signaled(i32),
}

impl Outcome {
    pub fn shell_code(self) -> i32 {
        match self {
            Self::Exited(code) => code,
            Self::Signaled(signal) => 128 + signal,
        }
    }
}

#[derive(Debug)]
pub struct Report {
    pub root_pid: i32,
    pub outcome: Outcome,
    pub exec_events: u64,
    pub redirected: u64,
    /// Targeted syscall stops for allocation, denial and vfork cleanup.
    pub injection_stops: u64,
}

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
mod linux;
#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
pub use linux::supervise;

/// Run synchronously on one dedicated OS thread. The callback runs on that
/// thread, with the execing task stopped. Returning an error kills and reaps the
/// supervised tree. Setting cancellation also kills and reaps that tree.
#[cfg(not(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
)))]
pub fn supervise(
    _command: Command,
    _cancel: &std::sync::atomic::AtomicBool,
    _policy: impl FnMut(&Exec) -> std::io::Result<Action>,
) -> std::io::Result<Report> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "exec interception requires Linux x86-64 or aarch64",
    ))
}
