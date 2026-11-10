//! Detach keys: the key sequence that makes a client leave a process on a
//! terminal without ending it, `ctrl-]` by default.
//!
//! The keys are written as Docker writes `--detach-keys`: keys separated by
//! commas, each a single character or `ctrl-` and one of `a`-`z`, `@`, `[`,
//! `\`, `]`, `^` and `_`. A key is recognized as the byte a terminal sends
//! for it and, for a control key, in the encodings terminals send once a
//! program turned on an enhanced keyboard mode, which reaches the client's
//! terminal through the program's output: the kitty keyboard protocol and
//! xterm's modifyOtherKeys. Keys inside a bracketed paste are never taken.

/// What [`Scanner::scan`] makes of input.
#[derive(Debug, PartialEq, Eq)]
pub struct Scanned {
  /// The input to forward, without the detach keys.
  pub forward: Vec<u8>,
  /// The detach keys were typed.
  pub detach: bool,
}

#[derive(Debug, thiserror::Error)]
#[error("invalid detach keys {keys:?}: {reason}")]
pub struct Invalid {
  keys: String,
  reason: String,
}

/// The detach key sequence and the input that may be part of it.
#[derive(Debug, Clone)]
pub struct Scanner {
  /// Every byte string that types the whole sequence.
  sequences: Vec<Vec<u8>>,
  /// Byte strings that type the first keys of a sequence; input that ends
  /// on one waits for the next read instead of being forwarded.
  heads: Vec<Vec<u8>>,
  pending: Vec<u8>,
  paste: bool,
  /// The last bytes seen, to find the paste's start and end markers.
  recent: Vec<u8>,
}

// Source: https://github.com/ThomasDickey/xterm-snapshots/blob/xterm-412/ctlseqs.txt#L1185
// (DEC private mode 2004) and the bracketed paste section: a paste arrives
// between `CSI 200 ~` and `CSI 201 ~`.
const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

impl Scanner {
  /// Parses `keys`; `None` for an empty string, which turns detaching off.
  pub fn parse(keys: &str) -> Result<Option<Self>, Invalid> {
    if keys.is_empty() {
      return Ok(None);
    }
    let invalid = |reason: &str| Invalid {
      keys: keys.into(),
      reason: reason.into(),
    };
    let mut per_key: Vec<Vec<Vec<u8>>> = Vec::new();
    for key in keys.split(',') {
      per_key.push(encodings(key).ok_or_else(|| invalid(&format!("{key:?} is not a character or ctrl-<key>")))?);
    }
    let mut sequences: Vec<Vec<u8>> = vec![Vec::new()];
    let mut heads = Vec::new();
    for (i, forms) in per_key.iter().enumerate() {
      sequences = sequences.iter().flat_map(|done| forms.iter().map(move |form| [done.as_slice(), form].concat())).collect();
      if i + 1 < per_key.len() {
        heads.extend(sequences.iter().cloned());
      }
    }
    Ok(Some(Self {
      sequences,
      heads,
      pending: Vec::new(),
      paste: false,
      recent: Vec::new(),
    }))
  }

  /// Takes the detach keys out of `input`. Bytes that may begin the
  /// sequence are held until they complete it or cannot; held bytes that
  /// do not end on a whole key are forwarded at the end of the read, since a
  /// terminal sends a key's encoding in one write.
  pub fn scan(&mut self, input: &[u8]) -> Scanned {
    let mut forward = Vec::with_capacity(input.len());
    for &byte in input {
      self.recent.push(byte);
      if self.recent.len() > PASTE_START.len() {
        self.recent.remove(0);
      }
      if self.paste {
        forward.push(byte);
        if self.recent.ends_with(PASTE_END) {
          self.paste = false;
        }
        continue;
      }
      self.pending.push(byte);
      if self.sequences.contains(&self.pending) {
        self.pending.clear();
        return Scanned {
          forward,
          detach: true,
        };
      }
      // Forward from the front until what is held may still begin the
      // sequence.
      while !self.pending.is_empty() && !self.sequences.iter().any(|s| s.starts_with(&self.pending)) {
        forward.push(self.pending.remove(0));
      }
      if self.recent.ends_with(PASTE_START) {
        forward.append(&mut self.pending);
        self.paste = true;
      }
    }
    if !self.heads.contains(&self.pending) {
      forward.append(&mut self.pending);
    }
    Scanned {
      forward,
      detach: false,
    }
  }
}

/// The byte strings a terminal sends for `key`.
fn encodings(key: &str) -> Option<Vec<Vec<u8>>> {
  let Some(name) = key.strip_prefix("ctrl-") else {
    let mut chars = key.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
      return None;
    };
    return Some(vec![c.to_string().into_bytes()]);
  };
  let [c] = name.as_bytes() else {
    return None;
  };
  let c = c.to_ascii_lowercase();
  if !(c.is_ascii_lowercase() || b"@[\\]^_".contains(&c)) {
    return None;
  }
  // NOTICE: kitty sends a control key as `CSI code ; 5 u` (5 is 1 + ctrl),
  // `CSI code ; 5 : 1 u` when it reports event types; modifyOtherKeys as
  // `CSI 27 ; 5 ; code ~`.
  // Source: https://github.com/kovidgoyal/kitty/blob/v0.50.0/docs/keyboard-protocol.rst?plain=1#L79-L130
  // and https://github.com/ThomasDickey/xterm-snapshots/blob/xterm-412/ctlseqs.txt (modifyOtherKeys).
  let code = u32::from(c);
  Some(vec![
    vec![c & 0x1f],
    format!("\x1b[{code};5u").into_bytes(),
    format!("\x1b[{code};5:1u").into_bytes(),
    format!("\x1b[27;5;{code}~").into_bytes(),
  ])
}

#[cfg(test)]
#[path = "keys_test.rs"]
mod tests;
