//! Process exit status, shared by every platform. Signals travel by name, for
//! example `SIGTERM`, because their numbers differ between platforms.

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Exit {
  /// Present when the process exited normally.
  pub code: Option<i32>,
  /// Signal name when a signal ended the process.
  pub signal: String,
}

impl std::fmt::Display for Exit {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self.code {
      Some(code) => write!(f, "exit {code}"),
      None if !self.signal.is_empty() => write!(f, "signal {}", self.signal),
      None => f.write_str("unknown"),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::Exit;

  #[test]
  fn exit_display_and_default() {
    assert_eq!(
      Exit::default(),
      Exit {
        code: None,
        signal: String::new()
      }
    );
    assert_eq!(Exit::default().to_string(), "unknown");
    assert_eq!(
      Exit {
        code: Some(0),
        signal: String::new(),
      }
      .to_string(),
      "exit 0"
    );
    assert_eq!(
      Exit {
        code: None,
        signal: "SIGTERM".into(),
      }
      .to_string(),
      "signal SIGTERM"
    );
  }
}
