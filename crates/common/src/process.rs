//! Process exit status, shared by every platform. Signals travel by name, for
//! example `SIGTERM`, because their numbers differ between platforms.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exit {
    /// Present when the process exited normally.
    pub code: Option<i32>,
    /// Signal name when a signal ended the process.
    pub signal: String,
}
