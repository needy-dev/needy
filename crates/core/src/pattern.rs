//! Command patterns, written like the commands they match: `git **`,
//! `git commit *`, `"{node,bun} {script} **"`. Rules, launcher resolvers,
//! `when.under` and `exec` faults all match commands through this module.
//!
//! | Token | Meaning |
//! | --- | --- |
//! | `git`, `*.sh` | One argument matching the glob |
//! | `*` | Exactly one argument |
//! | `**` | Zero or more arguments |
//! | `{a,b}` | One of the alternatives |
//! | `{name}` | Capture one argument |
//!
//! The first word of a pattern that has no `/` matches the command's
//! basename, so `git` matches `/usr/bin/git`; a path matches only that path.

use std::collections::BTreeMap;
use std::path::Path;

use glob_match::glob_match;

/// A compiled command pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
  tokens: Vec<Token>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
  Literal(String),
  One,
  Many,
  Alternatives(Vec<String>),
  Capture(String),
}

/// What a pattern matched.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Match {
  /// Arguments captured by `{name}` tokens.
  pub captures: BTreeMap<String, String>,
  /// Arguments matched by the last `**`.
  pub rest: Vec<String>,
}

impl Pattern {
  pub fn compile(text: &str) -> Pattern {
    let tokens = text
      .split_whitespace()
      .map(|word| match word {
        "*" => Token::One,
        "**" => Token::Many,
        _ => match word.strip_prefix('{').and_then(|w| w.strip_suffix('}')) {
          Some(inner) if inner.contains(',') => Token::Alternatives(inner.split(',').map(str::to_string).collect()),
          Some(inner) if !inner.is_empty() && inner.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') => {
            Token::Capture(inner.to_string())
          }
          _ => Token::Literal(word.to_string()),
        },
      })
      .collect();
    Pattern { tokens }
  }

  /// Matches a whole argument vector.
  pub fn matches(&self, argv: &[String]) -> Option<Match> {
    let mut m = Match::default();
    match_tokens(&self.tokens, argv, true, &mut m).then_some(m)
  }

  /// The command names the first word can match, when they are all literal;
  /// `None` when the pattern can match any command.
  pub fn command_names(&self) -> Option<Vec<String>> {
    let literal = |word: &String| !word.contains(['*', '?', '[', '{']);
    match self.tokens.first()? {
      Token::Literal(word) if literal(word) => Some(vec![word.clone()]),
      Token::Alternatives(words) if words.iter().all(literal) => Some(words.clone()),
      _ => None,
    }
  }
}

impl Match {
  /// Substitutes `{name}` captures in `words` and expands a `**` word into
  /// the arguments `**` matched.
  pub fn expand(&self, words: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for word in words {
      if word == "**" {
        out.extend(self.rest.iter().cloned());
        continue;
      }
      let mut text = word.clone();
      for (name, value) in &self.captures {
        text = text.replace(&format!("{{{name}}}"), value);
      }
      out.push(text);
    }
    out
  }
}

/// The program name of a command word: its last path component.
pub fn basename(word: &str) -> &str {
  Path::new(word).file_name().and_then(|name| name.to_str()).unwrap_or(word)
}

fn word_matches(pattern: &str, arg: &str, first: bool) -> bool {
  if first && !pattern.contains('/') {
    glob_match(pattern, basename(arg))
  } else {
    glob_match(pattern, arg)
  }
}

fn match_tokens(tokens: &[Token], argv: &[String], first: bool, m: &mut Match) -> bool {
  let Some(token) = tokens.first() else {
    return argv.is_empty();
  };
  if let Token::Many = token {
    for take in 0..=argv.len() {
      let mut attempt = m.clone();
      attempt.rest = argv[..take].to_vec();
      if match_tokens(&tokens[1..], &argv[take..], false, &mut attempt) {
        *m = attempt;
        return true;
      }
    }
    return false;
  }
  let Some(arg) = argv.first() else {
    return false;
  };
  let ok = match token {
    Token::Literal(p) => word_matches(p, arg, first),
    Token::Alternatives(ps) => ps.iter().any(|p| word_matches(p, arg, first)),
    Token::One => true,
    Token::Capture(name) => {
      m.captures.insert(name.clone(), arg.clone());
      true
    }
    Token::Many => unreachable!("handled above"),
  };
  ok && match_tokens(&tokens[1..], &argv[1..], false, m)
}

#[cfg(test)]
#[path = "pattern_test.rs"]
mod tests;
