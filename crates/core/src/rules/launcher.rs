//! Launcher resolution: the command a launcher such as `env`, `sudo` or
//! `uv run` really runs. User resolvers apply first, then the built-in table.

use needy_config::profile::Profile;

use super::shell::is_assignment;
use crate::pattern::{Pattern, basename};

/// How many rewrites one command may go through.
const RESOLVE_LIMIT: usize = 16;

/// Applies user resolvers (`patterns` are theirs, compiled), then the
/// built-in table, until neither changes the command.
pub(super) fn resolve(profile: &Profile, patterns: &[Pattern], argv: &[String]) -> Vec<String> {
  let mut current = argv.to_vec();
  'outer: for _ in 0..RESOLVE_LIMIT {
    for (resolver, pattern) in profile.resolvers.iter().zip(patterns) {
      if let Some(m) = pattern.matches(&current) {
        match &resolver.effective {
          Some(effective) => {
            let words: Vec<String> = effective.split_whitespace().map(str::to_string).collect();
            let next = m.expand(&words);
            if next != current && !next.is_empty() {
              current = next;
              continue 'outer;
            }
          }
          None => break 'outer,
        }
        break 'outer;
      }
    }
    match builtin(&current) {
      Some(next) if !next.is_empty() && next != current => current = next,
      _ => break,
    }
  }
  current
}

/// Index of the first argument after options; options in `with_value` take the
/// next argument unless written as `--opt=value`.
fn skip_options(args: &[String], with_value: &[&str], skip_assignments: bool) -> usize {
  let mut i = 0;
  while i < args.len() {
    let arg = args[i].as_str();
    if arg == "--" {
      return i + 1;
    }
    if arg.starts_with('-') && arg.len() > 1 {
      i += if with_value.contains(&arg) { 2 } else { 1 };
      continue;
    }
    if skip_assignments && is_assignment(arg) {
      i += 1;
      continue;
    }
    break;
  }
  i.min(args.len())
}

fn builtin(argv: &[String]) -> Option<Vec<String>> {
  let name = basename(argv.first()?);
  let args = &argv[1..];
  let rest = |i: usize| -> Option<Vec<String>> {
    let tail = args.get(i..)?;
    (!tail.is_empty()).then(|| tail.to_vec())
  };
  match name {
    "env" => rest(skip_options(args, &["-u", "--unset", "-C", "--chdir", "-S", "--split-string"], true)),
    "sudo" => rest(skip_options(
      args,
      &[
        "-u",
        "-g",
        "-C",
        "-D",
        "-h",
        "-p",
        "-r",
        "-t",
        "-U",
        "-T",
        "--user",
        "--group",
        "--chdir",
        "--prompt",
        "--role",
        "--type",
        "--host",
        "--other-user",
        "--close-from",
        "--command-timeout",
      ],
      true,
    )),
    "nice" => rest(skip_options(args, &["-n", "--adjustment"], false)),
    "ionice" => rest(skip_options(args, &["-c", "-n", "--class", "--classdata"], false)),
    "nohup" | "corepack" => rest(skip_options(args, &[], false)),
    "setsid" => rest(skip_options(args, &[], false)),
    "stdbuf" => rest(skip_options(args, &["-i", "-o", "-e"], false)),
    "time" => rest(skip_options(args, &["-f", "-o", "--format", "--output"], false)),
    "timeout" => {
      let i = skip_options(args, &["-s", "-k", "--signal", "--kill-after"], false);
      rest(i + 1)
    }
    "pnpm" if args.first().map(String::as_str) == Some("exec") => {
      rest(1 + skip_options(&args[1..], &["--filter", "-F", "-C", "--dir"], false))
    }
    "npm" if args.first().map(String::as_str) == Some("exec") => {
      let dash = args.iter().position(|a| a == "--")?;
      rest(dash + 1)
    }
    "uv" if args.first().map(String::as_str) == Some("run") => rest(
      1 + skip_options(
        &args[1..],
        &[
          "--with",
          "--python",
          "-p",
          "--project",
          "--directory",
          "--package",
          "--extra",
          "--group",
          "--env-file",
        ],
        false,
      ),
    ),
    _ => None,
  }
}
