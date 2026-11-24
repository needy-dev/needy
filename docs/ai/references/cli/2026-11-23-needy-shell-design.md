# `needy shell`: sandboxed commands in the user's shell

**Status:** draft. A first version is in `crates/cli/src/commands/shell/`.
On 2026-11-23 the owner approved three of its defaults: the prompt prefix,
the refusal of calls without a terminal, and support for zsh and bash only.
Every name in this note is **provisional**: `needy shell` and its
subcommands `init`, `activate` and `deactivate`, `NEEDY_SHELL_DISABLE_PROMPT`
and the term *sandboxed command*.

## Problem

To run a program in a sandbox today, the user types
`needy run create -it -f PROFILE -- ARGS`. The owner wants to type the
program's name in their own shell instead: a command that has a sandbox runs
in it, and every other command runs as usual. The user should notice as
little as possible. Needy shows no generated files, and leaving restores the
shell as it was.

## Solution

`needy shell` defines a **sandboxed command**: a shell function named after
the file name of the profile's `actor.command[0]`. Each call of the function
is one `needy run create -it -f PROFILE -- ARGS`. One activation script
defines the function, in a subshell or in the current shell:

```sh
# Subshell: the user's shell with the user's startup files; `exit` leaves it.
needy shell -f builtin:codex
needy shell -f builtin:codex --shell bash

# Current shell: no new process. The first line once per shell, or in ~/.zshrc.
eval "$(needy shell init zsh)"
needy shell activate -f builtin:codex
needy shell deactivate    # removes the function and restores the prompt
```

Inside either form:

```sh
codex --continue    # needy run create -it -f builtin:codex -- --continue
command codex       # the program on this machine, without a sandbox
```

A process cannot define a function in the shell that started it.
`needy shell init` therefore prints a `needy` shell function, which the shell
runs once with `eval`. That function runs the script that
`needy shell activate` prints, and handles `needy shell deactivate` itself.
It passes every other call to the program unchanged. The subshell defines the
same function, so `activate` and `deactivate` work there too.

## Decisions

| Decision | Reason |
| --- | --- |
| One run per call. | The call behaves as `run create -it` does: the `needy: run <id>` and workspace lines, `ctrl-]`, and the exit status are unchanged. A shared run is deferred (see [Out of scope](#out-of-scope)). |
| A shell function, not a directory on `PATH`. | A shell looks up functions before `PATH`. A startup file, direnv or mise that changes `PATH` later cannot bypass the function. Functions do not reach non-shell child processes, so the execution holder, which inherits the CLI's environment (`crates/cli/src/commands/run/execution.rs`), cannot find the function again. |
| The function's name is the file name of `actor.command[0]`. There is no map from names to profiles. | A profile has one entry from the host: its driving program. The user passes one profile with `-f`, and its driving program is the only sandboxed command. |
| The caller's arguments follow `actor.command`. | `run create` already appends them and never replaces the command (owner decision, 2026-11-12, `crates/daemon/src/run/mod.rs`). Fixed arguments such as `builtin:codex`'s `--sandbox` and `-c` flags therefore apply to every call. A profile whose fixed arguments are a task, such as `examples/opencode`'s `opencode run "<prompt>"`, is a poor fit, as it is for `run create -- ARGS`. |
| Only names that break the script are refused: `needy`, the builtins the script and `_needy_shell_deactivate` call, and reserved words. | The user chose the profile with `-f`, so a shell or a wrapper is a sandboxed command too: with `examples/linux-mac/bash.yaml`, `bash` opens bash in a sandbox, and `command bash` runs the program on this machine. A `needy` function would catch `needy` commands, a builtin's function would break the script, and a reserved word is read before any function. A name must also be one that zsh and bash accept without quotes. Two costs remain: a prompt or completion that runs, for example, `$(env ...)` calls the function there, which refuses the call without a terminal; and with `sh -c '<script>'`, `sh x.sh` runs the fixed script with `x.sh` as `$0`. |
| The profile is resolved locally. | `needy_config::profile::load` merges `extends` and reads `builtin:` profiles without a control daemon. |
| The function carries this invocation's global options. | `--config`, `--context`, `--address`, `--credential-file` and `--quiet` apply to every call. A profile file path and the `--config` and `--credential-file` paths become absolute, because the user may change directory before a call. A bare profile name or a `builtin:` profile stays as written. |
| The function runs `command needy`, not the absolute path of the running binary. | An upgrade through Homebrew or proto moves the binary to a new versioned directory, which would break a function that a long-lived shell keeps. |
| zsh subshell: `ZDOTDIR` points at a private directory whose `.zshenv` restores the user's `ZDOTDIR` first. | zsh then reads the user's own `.zshrc`, and `/etc/zshrc` sees the user's directory (macOS sets `HISTFILE` from `ZDOTDIR` there). The function is defined in a `precmd` hook that runs once, before the first prompt, after the user's `.zshrc`. Ghostty's zsh integration restores `ZDOTDIR` the same way. |
| bash subshell: `--rcfile` names a file that loads `~/.bashrc` first. | `--rcfile` replaces only `~/.bashrc`, so the file loads it before the activation script. |
| The startup file deletes its own directory. | The shell has already opened the file, so the deletion does not stop it. needy removes the directory again after the shell exits, for a shell that never read the file. |
| The subshell starts `$SHELL`; `--shell` names another shell. | `$SHELL` matches what most users expect. Its cost: `$SHELL` is the login shell, so a user who types `needy shell` in a temporary bash gets zsh unless they pass `--shell bash`. |
| The script exports nothing. | Functions do not reach a nested shell, so an exported marker would make a prompt show a sandboxed command where there is none. State lives in unexported variables (`_NEEDY_SHELL_PS1`). |
| The prompt gets a `(needy) ` prefix; `NEEDY_SHELL_DISABLE_PROMPT=1` turns it off. | The prefix shows that the program runs in a sandbox. Themes that rebuild the prompt before every prompt, such as Powerlevel10k and Starship, drop the prefix. Owner decision, 2026-11-23. |
| A call without a terminal on stdin and stdout is refused. | The run gets a PTY, so a pipe would read `\r\n` line endings, and a full-screen program could wait for input forever. The message names `command NAME` for the program on this machine. Owner decision, 2026-11-23. |
| Running the script again removes the function it defined before. | Activating another profile in the same shell leaves no old function behind. |
| Every form is a subcommand of `needy shell`; the subshell is `needy shell -f PROFILE`, without a verb. | needy's other top-level commands are nouns with a verb after them (`needy run create`). pixi (`pixi shell`), Hatch (`hatch shell`) and Pipenv (`pipenv shell`) also start a subshell with a bare `shell`. Its cost: `needy shell` takes `-f` and has subcommands, so its help shows both usages. Owner decision, 2026-11-24. |
| The current shell changes through the `needy` function from `needy shell init SHELL`. | The user writes `eval` once, in a startup file or the current terminal, and does not name the shell again. pyenv (`pyenv init`) defines its function the same way. The function looks only at the first two words: `needy --config X shell activate` reaches the program, which says to put options after `activate`. Owner decision, 2026-11-24. |
| `needy shell activate` or `deactivate` that reaches the program fails and names `needy shell init`. | The function handles both, so the program sees them only when the shell has no function. `mise shell` and `conda activate` report the same case. The function passes a hidden `--print SHELL` to get the script. Owner decision, 2026-11-24. |
| `needy shell init` on a terminal prints how to use it, not the code. | Code shown on a terminal does nothing, so the user most likely left out `eval`. A pipe still gets the code: `needy shell init zsh \| less`. Owner decision, 2026-11-24. |
| The function passes `--help` after `needy shell activate` to the program. | Otherwise the function would run the help text as shell code. |
| The script's own exit function is `_needy_shell_deactivate`; users type `needy shell deactivate`. | Python's venv, conda and Poetry define `deactivate`, so the script keeps out of that name. |

## Known limits

- A startup file that ends in `exec` (for example `exec tmux`) never reaches
  the activation script. That shell has no sandboxed command, and the program
  runs on this machine without a warning.
- The subshell is not a login shell. `.zprofile` and `.bash_profile` do not
  run, as when the user types `zsh` or `bash`. A macOS bash user whose aliases
  are only in `~/.bash_profile` does not get them.
- A bash built with `SYS_BASHRC` (Debian's `/etc/bash.bashrc`) still reads
  that file before the `--rcfile` file, per `run_startup_files` in bash's
  `shell.c`. This is not checked on Linux.
- A `.zshrc` that assigns `precmd_functions` instead of appending to it
  removes the activation hook.
- Scripts, Makefiles and editors that start the program through `PATH` run
  the program on this machine. Only commands the user types in the shell are
  sandboxed.

## Testing

- `crates/cli/src/commands/shell/mod_test.rs`: the function's name, refused
  names, and the shell a program path names.
- `crates/cli/tests/shell.rs`, against the built `needy`:
  - `needy shell activate --print` prints a script that defines the
    function, and names it on stderr;
  - through the `needy` function, in bash and zsh, `needy shell activate`
    defines the function and adds the prompt prefix, other calls reach the
    program, and `needy shell deactivate` removes both;
  - the function prints `--help` instead of running it, and
    `needy shell deactivate` with nothing active says so;
  - `needy shell activate` without the function fails and names
    `needy shell init`; `needy shell init` on a terminal prints how to use it;
  - a call on a terminal runs `needy run create -it -f PROFILE -- ARGS` with
    absolute paths and the global options (a stand-in `needy` records its
    arguments);
  - a named or `builtin:` profile stays a name;
  - a call without a terminal is refused before `needy` runs;
  - an alias of the same name does not shadow the function;
  - a second activation removes the first function;
  - a shell as the driving program becomes a sandboxed command, and `needy`
    is refused;
  - the subshell refuses another shell and needs a terminal;
  - the zsh and bash subshells run the user's startup files first, define the
    sandboxed command and the `needy` function, leave no startup directory behind, and exit with the shell's
    status; in zsh, `ZDOTDIR` is restored and `HISTFILE` is outside the
    startup directory.
- The zsh tests pass without checking anything on a host without zsh, and
  they say so on stderr.
- Measured on macOS on Apple silicon with zsh 5.9 and bash 3.2.57. The tests
  have not run on Linux.

Measurements behind the shared-run deferral, with each shell on a PTY:

| How the shell ends | zsh `zshexit` | bash `trap EXIT` |
| --- | --- | --- |
| SIGHUP | runs | runs |
| The terminal closes | runs | runs |
| `exec` of another program | does not run | does not run |
| SIGKILL | does not run | does not run |

## Out of scope

Each item has a `TODO(needy-shell)` marker at its code site.

- **A shared run**: one run for a whole session, each call a new process in
  the same actor. A driving program's exit ends its run, so this needs an
  idle actor, terminal processes started in the actor from the host, and
  per-call records in the report. The state of a shared run reaches the host
  only when the run stops, because stop hooks, mirror-out and sync merges run
  after the driving program exits. In the current shell, `needy shell deactivate`
  and an exit hook cover most endings, but not `exec` or SIGKILL (see the
  table above), so the run also needs a watcher process or an idle timeout.
- **Several profiles in one session**, or a map from command names to
  profiles in the global configuration.
- **fish**, which needs its own script syntax. zsh and bash only is an owner
  decision (2026-11-23).
- **Calls without a terminal.**
- **A marker in executor processes**: an activation in `.zshrc` also runs in
  an interactive zsh that a profile routes to a native executor on this
  machine, where a call starts a nested run. The owner chose to leave it
  for now (2026-11-24).
- **Programs started through `PATH`** by scripts and editors, which would need
  a directory of shims in a fixed place.

## Open questions

- The names listed under **Status**.
