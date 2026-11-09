//! Process exit status and POSIX signal names, shared by every platform.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exit {
    pub code: Option<i32>,
    pub signal: String,
}

impl Exit {
    /// Shell-style status: the code, or 128 plus the signal number.
    pub fn shell_code(&self) -> i32 {
        match self.code {
            Some(code) => code,
            None => 128 + signal_number(&self.signal).unwrap_or(1),
        }
    }
}

const SIGNALS: &[(&str, i32)] = &[
    ("SIGHUP", 1),
    ("SIGINT", 2),
    ("SIGQUIT", 3),
    ("SIGILL", 4),
    ("SIGTRAP", 5),
    ("SIGABRT", 6),
    ("SIGKILL", 9),
    ("SIGSEGV", 11),
    ("SIGPIPE", 13),
    ("SIGALRM", 14),
    ("SIGTERM", 15),
];

pub fn signal_name(sig: i32) -> String {
    SIGNALS
        .iter()
        .find(|(_, n)| *n == sig)
        .map(|(name, _)| name.to_string())
        .unwrap_or_else(|| format!("SIG{sig}"))
}

pub fn signal_number(name: &str) -> Option<i32> {
    SIGNALS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, sig)| *sig)
}
