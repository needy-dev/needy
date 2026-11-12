//! What a terminal sends for keys, pastes and mouse events, given the modes
//! the program turned on: application cursor keys, the kitty keyboard
//! protocol's flags, bracketed paste and the mouse protocol.

use anyhow::{Result, bail};

// Source: https://github.com/kovidgoyal/kitty/blob/v0.50.0/docs/keyboard-protocol.rst?plain=1
// (progressive enhancement: flag 1 disambiguates escape codes, so Escape,
// and keys with Ctrl or Alt, are sent as `CSI code ; modifiers u`; flag 8
// reports every key, Enter, Tab and Backspace too, as escape codes; the
// modifiers are 1 + shift 1 + alt 2 + ctrl 4).
// Source: https://github.com/ThomasDickey/xterm-snapshots/blob/xterm-412/ctlseqs.txt
// (cursor keys `CSI A` .. `CSI D`, `SS3 A` .. `SS3 D` with DECCKM; `CSI 2 0 0 ~`
// and `CSI 2 0 1 ~` around a bracketed paste; SGR mouse `CSI < b ; x ; y M`
// or `m` on release; X10 mouse `CSI M` and three bytes of 32 + value),
// xterm patch 412.
const DISAMBIGUATE: u16 = 1;
const ALL_KEYS: u16 = 8;

/// The bytes a terminal sends for the key `name`: `enter`, `up`, `ctrl-c`,
/// `alt-x`, `esc`, ...
pub fn key(name: &str, application_cursor: bool, flags: u16) -> Result<Vec<u8>> {
  let csi_u = |code: u32, modifiers: u32| -> Vec<u8> {
    if modifiers == 1 {
      format!("\x1b[{code}u").into_bytes()
    } else {
      format!("\x1b[{code};{modifiers}u").into_bytes()
    }
  };
  let cursor = |c: char| -> Vec<u8> {
    if application_cursor {
      format!("\x1bO{c}").into_bytes()
    } else {
      format!("\x1b[{c}").into_bytes()
    }
  };
  let bytes = match name {
    "enter" if flags & ALL_KEYS != 0 => csi_u(13, 1),
    "enter" => b"\r".to_vec(),
    "tab" if flags & ALL_KEYS != 0 => csi_u(9, 1),
    "tab" => b"\t".to_vec(),
    "shift-tab" => b"\x1b[Z".to_vec(),
    "backspace" if flags & ALL_KEYS != 0 => csi_u(127, 1),
    "backspace" => b"\x7f".to_vec(),
    "esc" | "escape" if flags & DISAMBIGUATE != 0 => csi_u(27, 1),
    "esc" | "escape" => b"\x1b".to_vec(),
    "space" => b" ".to_vec(),
    "up" => cursor('A'),
    "down" => cursor('B'),
    "right" => cursor('C'),
    "left" => cursor('D'),
    "home" => cursor('H'),
    "end" => cursor('F'),
    "pgup" => b"\x1b[5~".to_vec(),
    "pgdn" => b"\x1b[6~".to_vec(),
    "delete" => b"\x1b[3~".to_vec(),
    other => {
      let (modifiers, rest) = if let Some(rest) = other.strip_prefix("ctrl-") {
        (5, rest)
      } else if let Some(rest) = other.strip_prefix("alt-") {
        (3, rest)
      } else {
        bail!("unknown key {other:?}")
      };
      let mut chars = rest.chars();
      let (Some(c), None) = (chars.next(), chars.next()) else {
        bail!("unknown key {other:?}")
      };
      if flags & DISAMBIGUATE != 0 {
        csi_u(u32::from(c.to_ascii_lowercase()), modifiers)
      } else if modifiers == 5 {
        let byte = u8::try_from(u32::from(c.to_ascii_lowercase())).unwrap_or(0);
        if !(byte.is_ascii_lowercase() || b"@[\\]^_".contains(&byte)) {
          bail!("no control code for {other:?}")
        }
        vec![byte & 0x1f]
      } else {
        let mut bytes = vec![0x1b];
        bytes.extend_from_slice(c.to_string().as_bytes());
        bytes
      }
    }
  };
  Ok(bytes)
}

/// Typed text: as itself, or one escape code per key when the program asked
/// for every key as an escape code.
pub fn text(text: &str, flags: u16) -> Vec<u8> {
  if flags & ALL_KEYS == 0 {
    return text.as_bytes().to_vec();
  }
  text.chars().map(|c| format!("\x1b[{}u", u32::from(c))).collect::<String>().into_bytes()
}

/// A paste: bracketed when the program turned bracketed paste on. A
/// terminal sends a newline in a paste as a carriage return.
pub fn paste(text: &str, bracketed: bool) -> Vec<u8> {
  let body = text.replace("\r\n", "\r").replace('\n', "\r");
  if bracketed {
    format!("\x1b[200~{body}\x1b[201~").into_bytes()
  } else {
    body.into_bytes()
  }
}

/// A mouse event at 1-based `row` and `col`: `click` (press and release of
/// the left button), `scroll-up` or `scroll-down`. `None` when the program
/// has no mouse reporting on, as a terminal then sends nothing.
pub fn mouse(action: &str, row: u16, col: u16, screen: &vt100::Screen) -> Result<Option<Vec<u8>>> {
  if screen.mouse_protocol_mode() == vt100::MouseProtocolMode::None {
    return Ok(None);
  }
  let events: &[(u16, bool)] = match action {
    "click" => &[(0, true), (0, false)],
    "scroll-up" => &[(64, true)],
    "scroll-down" => &[(65, true)],
    other => bail!("unknown mouse action {other:?}"),
  };
  let mut bytes = Vec::new();
  for &(button, press) in events {
    if screen.mouse_protocol_encoding() == vt100::MouseProtocolEncoding::Sgr {
      let end = if press { 'M' } else { 'm' };
      bytes.extend_from_slice(format!("\x1b[<{button};{col};{row}{end}").as_bytes());
    } else {
      let button = if press { button } else { 3 };
      let byte = |v: u16| u8::try_from(32 + v).unwrap_or(255);
      bytes.extend_from_slice(&[0x1b, b'[', b'M', byte(button), byte(col), byte(row)]);
    }
  }
  Ok(Some(bytes))
}

/// `text` with `\r`, `\n`, `\t`, `\e`, `\xNN` and `\\` taken as escapes.
pub fn unescape(text: &str) -> Result<Vec<u8>> {
  let mut out = Vec::new();
  let mut chars = text.chars();
  while let Some(c) = chars.next() {
    if c != '\\' {
      let mut buf = [0; 4];
      out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
      continue;
    }
    match chars.next() {
      Some('r') => out.push(b'\r'),
      Some('n') => out.push(b'\n'),
      Some('t') => out.push(b'\t'),
      Some('e') => out.push(0x1b),
      Some('\\') => out.push(b'\\'),
      Some('x') => {
        let hex: String = chars.by_ref().take(2).collect();
        out.push(u8::from_str_radix(&hex, 16)?);
      }
      other => bail!("unknown escape \\{}", other.map(String::from).unwrap_or_default()),
    }
  }
  Ok(out)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn keys_follow_the_programs_modes() {
    assert_eq!(key("up", false, 0).unwrap(), b"\x1b[A");
    assert_eq!(key("up", true, 0).unwrap(), b"\x1bOA");
    assert_eq!(key("ctrl-c", false, 0).unwrap(), b"\x03");
    assert_eq!(key("ctrl-c", false, DISAMBIGUATE).unwrap(), b"\x1b[99;5u");
    assert_eq!(key("alt-x", false, 0).unwrap(), b"\x1bx");
    assert_eq!(key("esc", false, DISAMBIGUATE).unwrap(), b"\x1b[27u");
    assert_eq!(key("enter", false, DISAMBIGUATE).unwrap(), b"\r");
    assert_eq!(key("enter", false, DISAMBIGUATE | ALL_KEYS).unwrap(), b"\x1b[13u");
  }

  #[test]
  fn a_paste_is_bracketed_only_when_the_program_asked() {
    assert_eq!(paste("a\nb", true), b"\x1b[200~a\rb\x1b[201~");
    assert_eq!(paste("a\nb", false), b"a\rb");
  }
}
