//! `sh -c SCRIPT` and similar: finding the script and splitting a word-only
//! script into commands.

// TODO(rules): the profile note proposes `brush-parser` instead of this small
// lexer; it stays until the owner decides (module layout redesign, open
// question "Shell parsing in rules").

use crate::pattern::basename;

/// Shells whose `-c` scripts are parsed. When a rule routes any command,
/// they are routed too, so a script that runs it is asked about.
pub(super) const SHELLS: &[&str] = &["sh", "bash", "dash", "zsh", "ash"];

/// The script of `sh -c SCRIPT`, `bash -lc SCRIPT` and similar.
pub(super) fn script(argv: &[String]) -> Option<&str> {
  let name = basename(argv.first()?);
  if !SHELLS.contains(&name) {
    return None;
  }
  let mut command_flag = false;
  for arg in &argv[1..] {
    if arg == "--" {
      continue;
    }
    if let Some(flags) = arg.strip_prefix('-')
      && !flags.starts_with('-')
      && !flags.is_empty()
    {
      command_flag |= flags.contains('c');
      continue;
    }
    if arg.starts_with("--") {
      continue;
    }
    return command_flag.then_some(arg.as_str());
  }
  None
}

/// Splits a word-only script (plain commands joined by `&&`, `||`, `;`, `|`
/// and newlines) into argument vectors. Returns `None` for anything else:
/// expansions, substitutions, redirects, grouping, background jobs.
pub(super) fn word_only(script: &str) -> Option<Vec<Vec<String>>> {
  let mut commands: Vec<Vec<String>> = Vec::new();
  let mut current: Vec<String> = Vec::new();
  let mut word = String::new();
  let mut in_word = false;
  let mut chars = script.chars().peekable();

  fn end_word(word: &mut String, in_word: &mut bool, current: &mut Vec<String>) {
    if *in_word {
      current.push(std::mem::take(word));
      *in_word = false;
    }
  }
  fn end_command(current: &mut Vec<String>, commands: &mut Vec<Vec<String>>) -> Option<()> {
    // Leading NAME=value assignments only set the environment.
    let start = current.iter().position(|w| !is_assignment(w)).unwrap_or(current.len());
    let argv = current.split_off(start);
    current.clear();
    if argv.is_empty() {
      return None;
    }
    commands.push(argv);
    Some(())
  }

  while let Some(c) = chars.next() {
    match c {
      '\'' => {
        in_word = true;
        loop {
          match chars.next()? {
            '\'' => break,
            ch => word.push(ch),
          }
        }
      }
      '"' => {
        in_word = true;
        loop {
          match chars.next()? {
            '"' => break,
            '$' | '`' => return None,
            '\\' => word.push(chars.next()?),
            ch => word.push(ch),
          }
        }
      }
      '\\' => {
        in_word = true;
        match chars.next()? {
          '\n' => {}
          ch => word.push(ch),
        }
      }
      ' ' | '\t' => end_word(&mut word, &mut in_word, &mut current),
      ';' | '\n' => {
        end_word(&mut word, &mut in_word, &mut current);
        if !current.is_empty() {
          end_command(&mut current, &mut commands)?;
        }
      }
      '|' | '&' => {
        if c == '&' && chars.peek() != Some(&'&') {
          return None;
        }
        if chars.peek() == Some(&c) {
          chars.next();
        }
        end_word(&mut word, &mut in_word, &mut current);
        end_command(&mut current, &mut commands)?;
      }
      '$' | '`' | '(' | ')' | '<' | '>' | '{' | '}' | '#' => return None,
      ch => {
        in_word = true;
        word.push(ch);
      }
    }
  }
  end_word(&mut word, &mut in_word, &mut current);
  if !current.is_empty() {
    end_command(&mut current, &mut commands)?;
  }
  (!commands.is_empty()).then_some(commands)
}

pub(super) fn is_assignment(word: &str) -> bool {
  match word.split_once('=') {
    Some((name, _)) => {
      !name.is_empty() && !name.starts_with(|c: char| c.is_ascii_digit()) && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    }
    None => false,
  }
}

#[cfg(test)]
#[path = "shell_test.rs"]
mod tests;
