//! A rule's `when` conditions, which are not positional.

use needy_config::profile::When;

use crate::intercept::ExecContext;
use crate::pattern::{Match, Pattern};

/// Whether every condition holds for a command whose effective arguments are
/// `argv` and whose pattern matched `m`. `under` is `when.under`, compiled.
pub(super) fn holds(when: &When, under: Option<&Pattern>, m: &Match, ctx: &ExecContext, argv: &[String]) -> bool {
  let args = argv.get(1..).unwrap_or_default();
  if !when.has.is_empty()
    && !when.has.iter().any(|flag| args.iter().any(|a| a == flag || (flag.starts_with("--") && a.starts_with(&format!("{flag}=")))))
  {
    return false;
  }
  if let Some(cwd) = &when.cwd
    && !cwd.matches(&ctx.cwd)
  {
    return false;
  }
  if when.env.iter().any(|(k, v)| ctx.env.get(k).map(String::as_str) != Some(v.as_str())) {
    return false;
  }
  if let Some(under) = under
    && !ctx.ancestors.iter().any(|a| under.matches(a).is_some())
  {
    return false;
  }
  if let Some(tty) = when.tty
    && tty != ctx.tty
  {
    return false;
  }
  when.captures.iter().all(|(name, glob)| m.captures.get(name).is_some_and(|value| glob.matches(value)))
}
