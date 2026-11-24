//! The shell code `needy shell` writes: the `needy` shell function that
//! `needy shell init` prints, the activation script that defines a sandboxed
//! command, and the startup files that run both in a subshell after the
//! user's own startup files.

use super::{SandboxedCommand, Shell};

/// Quotes a word for zsh and bash. A word of only letters, digits and
/// `_./:@+,-` stays bare; any other word goes in single quotes.
// NOTICE: `shell_words::quote` leaves `{`, `}`, `^` and `!` bare. zsh expands
// `{a,b}`, and `^` is a glob character under zsh's `EXTENDED_GLOB`, so this
// quotes every word outside a smaller set instead.
fn quote(word: &str) -> String {
  if !word.is_empty() && word.chars().all(|c| c.is_ascii_alphanumeric() || "_./:@+,-".contains(c)) {
    word.to_string()
  } else {
    format!("'{}'", word.replace('\'', r"'\''"))
  }
}

/// The `needy` shell function that `needy shell init` prints. zsh and bash
/// share it. It runs the activation script that `needy shell activate`
/// prints, and the `_needy_shell_deactivate` that script defined for
/// `needy shell deactivate`, because a process cannot change the shell that
/// started it. Every other call runs the program unchanged.
///
/// It sees only calls that start with `shell activate` or
/// `shell deactivate`; the program reports any other spelling of them.
/// `--help` passes through, so that help is printed, not run.
pub(super) fn init(shell: Shell) -> String {
  let shell = shell.name();
  format!(
    r#"# needy shell init: `needy shell activate` and `needy shell deactivate` change this shell.
function needy {{
  if [ "${{1-}}" = shell ] && [ "${{2-}}" = activate ]; then
    shift 2
    for _needy_shell_arg in "$@"; do
      case $_needy_shell_arg in
        -h|--help) builtin unset _needy_shell_arg; command needy shell activate "$@"; return;;
      esac
    done
    builtin unset _needy_shell_arg
    builtin typeset _needy_shell_script
    _needy_shell_script=$(command needy shell activate "$@" --print {shell}) || return
    eval "$_needy_shell_script"
  elif [ "${{1-}}" = shell ] && [ "${{2-}}" = deactivate ] && [ "$#" -eq 2 ]; then
    if builtin typeset -f _needy_shell_deactivate >/dev/null 2>&1; then
      _needy_shell_deactivate
    else
      builtin printf '%s\n' 'needy: no sandboxed command is active in this shell' >&2
      return 1
    fi
  else
    command needy "$@"
  fi
}}
"#
  )
}

/// The activation script. zsh and bash share it: it defines the sandboxed
/// command's function and `_needy_shell_deactivate`, which removes both and
/// restores the prompt. Running it again first removes the function it
/// defined before.
///
/// It exports nothing. Functions do not reach a nested shell or a script, so
/// an exported marker would claim a sandboxed command where there is none.
// TODO(needy-shell): a call without a terminal on stdin and stdout is
// refused, because the run gets a PTY and a pipe would read its `\r\n` line
// endings. The owner chose the refusal for this version (2026-11-23);
// running those calls waits for an owner request to support pipes.
// TODO(needy-shell): an activation in `.zshrc` also runs in an interactive
// zsh that a profile routes to a native executor on this machine, where a
// call starts a nested run. Executors set no `NEEDY_*` marker that the script
// could check; adding one is an executor change, which the owner chose to
// leave for now (2026-11-24).
pub(super) fn activation(command: &SandboxedCommand) -> String {
  let name = &command.name;
  let invocation = command.invocation.iter().map(|word| quote(word)).collect::<Vec<_>>().join(" ");
  let refusal = quote(&format!("needy: {name} runs in a sandbox only from a terminal; `command {name}` runs the program on this machine"));
  format!(
    r#"# needy shell: `{name}` runs in a sandbox; `needy shell deactivate` restores it.
if typeset -f _needy_shell_deactivate >/dev/null 2>&1; then _needy_shell_deactivate; fi
builtin unalias {name} 2>/dev/null
function {name} {{
  if [ ! -t 0 ] || [ ! -t 1 ]; then
    builtin printf '%s\n' {refusal} >&2
    return 1
  fi
  command {invocation} "$@"
}}
function _needy_shell_deactivate {{
  builtin unset -f {name} _needy_shell_deactivate
  if [ -n "${{_NEEDY_SHELL_PS1+x}}" ]; then
    PS1=$_NEEDY_SHELL_PS1
    builtin unset _NEEDY_SHELL_PS1
  fi
}}
if [ -z "${{NEEDY_SHELL_DISABLE_PROMPT-}}" ]; then
  _NEEDY_SHELL_PS1=$PS1
  PS1="(needy) $PS1"
fi
"#
  )
}

/// zsh's `.zshenv` for a subshell. needy points `ZDOTDIR` at the directory
/// that holds it and passes the user's own `ZDOTDIR`, if any, in
/// `NEEDY_SHELL_ZDOTDIR`.
///
/// The file restores the user's `ZDOTDIR` before anything else, so zsh then
/// reads the user's `.zshrc`, and `/etc/zshrc` sees the user's directory
/// (macOS sets `HISTFILE` from it). It deletes its own directory, which zsh
/// no longer reads, and defines the `needy` function and the sandboxed
/// command in a `precmd` hook that runs once, before the first prompt, after
/// the user's `.zshrc`.
// Source: Ghostty restores ZDOTDIR the same way, in
// `Ghostty.app/Contents/Resources/ghostty/shell-integration/zsh/.zshenv`
// (started as a copy of kitty's zsh integration).
// NOTICE: a `.zshrc` that ends in `exec tmux` (or any `exec`) never reaches
// the first prompt, so that shell has no sandboxed command, and the program runs on this
// machine. Removing a hook while zsh runs the hooks does not skip the next
// one (checked on zsh 5.9).
pub(super) fn zshenv(dir: &str, command: &SandboxedCommand) -> String {
  let dir = quote(dir);
  let activation = quote(&format!("{}{}", init(Shell::Zsh), activation(command)));
  format!(
    r#"# needy shell: zsh reads this file because needy pointed ZDOTDIR here.
if [[ -n "${{NEEDY_SHELL_ZDOTDIR+X}}" ]]; then
  builtin export ZDOTDIR="$NEEDY_SHELL_ZDOTDIR"
  builtin unset NEEDY_SHELL_ZDOTDIR
else
  builtin unset ZDOTDIR
fi
command rm -rf -- {dir}
{{
  builtin typeset _needy_shell_file=${{ZDOTDIR-$HOME}}/.zshenv
  [[ ! -r "$_needy_shell_file" ]] || builtin source -- "$_needy_shell_file"
}} always {{
  builtin unset _needy_shell_file
  function _needy_shell_activate {{
    precmd_functions=(${{precmd_functions:#_needy_shell_activate}})
    builtin unset -f _needy_shell_activate
    builtin eval {activation}
  }}
  precmd_functions+=(_needy_shell_activate)
}}
"#
  )
}

/// bash's startup file for a subshell, given with `--rcfile`, which bash
/// reads instead of `~/.bashrc`. It deletes its own directory, which bash no
/// longer reads, loads `~/.bashrc`, and defines the `needy` function and the
/// sandboxed command.
// NOTICE: `--rcfile` replaces only `~/.bashrc`. A bash built with
// `SYS_BASHRC` (Debian's `/etc/bash.bashrc`) still reads that file first,
// per `run_startup_files` in bash's `shell.c`; not checked on Linux. The
// subshell is not a login shell, so `~/.bash_profile` does not run, as when
// the user types `bash`.
pub(super) fn bashrc(dir: &str, command: &SandboxedCommand) -> String {
  format!(
    r#"# needy shell: bash reads this file instead of ~/.bashrc (--rcfile).
command rm -rf -- {dir}
if [ -r "$HOME/.bashrc" ]; then . "$HOME/.bashrc"; fi
{init}{activation}"#,
    dir = quote(dir),
    init = init(Shell::Bash),
    activation = activation(command)
  )
}
