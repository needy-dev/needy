//! Observable behavior of `needy shell`: the `needy` shell function and the
//! activation script it prints, what a call of the sandboxed command runs,
//! and how the subshell starts.

#![cfg(unix)]

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A temporary directory for one test: needy's directories, profiles, a
/// home for the shells and a stand-in `needy`.
struct Home {
  dir: tempfile::TempDir,
}

impl Home {
  fn new() -> Self {
    let dir = tempfile::Builder::new().prefix("needy-test-").tempdir().unwrap();
    Self { dir }
  }

  fn path(&self) -> &Path {
    self.dir.path()
  }

  fn write(&self, name: &str, text: &str) -> PathBuf {
    let path = self.path().join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, text).unwrap();
    path
  }

  /// A profile file whose driving program is `command`.
  fn profile(&self, name: &str, command: &str) -> PathBuf {
    self.write(name, &format!("user: {{ name: agent, uid: 1000, gid: 1000 }}\nactor:\n  driver: linux-firecracker\n  command: {command}\n"))
  }

  /// `needy` with only this directory's needy directories in its environment.
  fn needy(&self) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_needy"));
    for name in [
      "NEEDY_ENDPOINT",
      "NEEDY_EXECUTION_CONTEXT",
      "NEEDY_TOKEN",
      "NEEDY_RUN",
      "XDG_CONFIG_HOME",
    ] {
      command.env_remove(name);
    }
    command
      .env("NEEDY_CONFIG_DIR", self.path().join("config"))
      .env("NEEDY_STATE_DIR", self.path().join("state"))
      .env("NEEDY_DATA_DIR", self.path().join("data"))
      .env("NEEDY_RUNTIME_DIR", self.path().join("run"))
      .stdin(Stdio::null());
    command
  }

  /// The activation script that `needy shell activate` prints for the
  /// `needy` function in bash, which passes `--print`.
  fn activation(&self, profile: &Path) -> String {
    let output = self.needy().args(["shell", "activate", "--print", "bash", "-f"]).arg(profile).output().unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    stdout(&output)
  }

  /// The `needy` shell function that `needy shell init` prints.
  fn init(&self, shell: &str) -> String {
    let output = self.needy().args(["shell", "init", shell]).output().unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    stdout(&output)
  }

  /// `shell` without a terminal, with this directory's environment and the
  /// built `needy` first on `PATH`, running `script`.
  fn shell_with_needy(&self, shell: &str, script: &str) -> Output {
    let bin = Path::new(env!("CARGO_BIN_EXE_needy")).parent().unwrap();
    let mut command = Command::new(shell);
    command.env_clear().envs(self.shell_env());
    let mut path = OsString::from(bin);
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap_or_default());
    let flags: &[&str] = if shell == "zsh" {
      &["-f"]
    } else {
      &["--norc", "--noprofile"]
    };
    command.env("PATH", path).args(flags).args(["-c", script]).current_dir(self.path()).stdin(Stdio::null()).output().unwrap()
  }

  /// A directory with a stand-in `needy` that writes its arguments, one per
  /// line, to `$NEEDY_ARGS_FILE`.
  fn stand_in_needy(&self) -> PathBuf {
    let bin = self.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    needy_common::testing::write_executable(&bin.join("needy"), "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$NEEDY_ARGS_FILE\"\n").unwrap();
    bin
  }

  /// The environment of a shell on a terminal: this directory's `h` as its
  /// home, `tmp` as its temporary directory, and needy's directories.
  fn shell_env(&self) -> Vec<(String, OsString)> {
    std::fs::create_dir_all(self.path().join("h")).unwrap();
    std::fs::create_dir_all(self.path().join("tmp")).unwrap();
    vec![
      ("PATH".into(), std::env::var_os("PATH").unwrap_or_default()),
      ("HOME".into(), self.path().join("h").into()),
      ("TMPDIR".into(), self.path().join("tmp").into()),
      ("TERM".into(), "xterm-256color".into()),
      ("BASH_SILENCE_DEPRECATION_WARNING".into(), "1".into()),
      ("NEEDY_CONFIG_DIR".into(), self.path().join("config").into()),
      ("NEEDY_STATE_DIR".into(), self.path().join("state").into()),
      ("NEEDY_DATA_DIR".into(), self.path().join("data").into()),
      ("NEEDY_RUNTIME_DIR".into(), self.path().join("run").into()),
    ]
  }
}

fn stdout(output: &Output) -> String {
  String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
  String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Runs bash without startup files on `script` with the activation scripts
/// as `$1`, `$2`, ..., without a terminal.
fn bash(script: &str, activations: &[&str]) -> Output {
  Command::new("bash").args(["--norc", "--noprofile", "-c", script, "bash"]).args(activations).stdin(Stdio::null()).output().unwrap()
}

/// Whether a shell is installed. zsh is not on every CI host; the zsh tests
/// say so on stderr and pass there.
fn installed(shell: &str) -> bool {
  let found = Command::new(shell).args(["-c", "true"]).stdin(Stdio::null()).status().is_ok_and(|status| status.success());
  if !found {
    eprintln!("{shell} is not installed; this test checks nothing on this host");
  }
  found
}

/// Runs a program on a new terminal. Once `ready` appears in its output, types
/// `input`; then waits for the program to exit. Returns its output and exit
/// code.
fn on_terminal(argv: &[OsString], env: &[(String, OsString)], ready: Option<&str>, input: &str) -> (String, u32) {
  use portable_pty::{CommandBuilder, PtySize, native_pty_system};
  let pair = native_pty_system()
    .openpty(PtySize {
      // Wide enough that a typed line never wraps into a line of its own.
      rows: 40,
      cols: 1000,
      pixel_width: 0,
      pixel_height: 0,
    })
    .unwrap();
  let mut command = CommandBuilder::from_argv(argv.to_vec());
  command.env_clear();
  for (name, value) in env {
    command.env(name, value);
  }
  let mut child = pair.slave.spawn_command(command).unwrap();
  drop(pair.slave);
  let mut reader = pair.master.try_clone_reader().unwrap();
  let written = Arc::new(Mutex::new(Vec::new()));
  let sink = written.clone();
  let drain = std::thread::spawn(move || {
    let mut buf = [0u8; 4096];
    while let Ok(n) = reader.read(&mut buf) {
      if n == 0 {
        break;
      }
      sink.lock().unwrap().extend_from_slice(&buf[..n]);
    }
  });
  let text = || String::from_utf8_lossy(&written.lock().unwrap()).into_owned();
  let deadline = Instant::now() + Duration::from_secs(30);
  if let Some(ready) = ready {
    while !text().contains(ready) {
      assert!(Instant::now() < deadline, "no {ready:?} on the terminal:\n{}", text());
      std::thread::sleep(Duration::from_millis(20));
    }
  }
  let mut writer = pair.master.take_writer().unwrap();
  writer.write_all(input.as_bytes()).unwrap();
  writer.flush().unwrap();
  let status = loop {
    if let Some(status) = child.try_wait().unwrap() {
      break status;
    }
    if Instant::now() > deadline {
      child.kill().unwrap();
      panic!("the program did not exit:\n{}", text());
    }
    std::thread::sleep(Duration::from_millis(20));
  };
  // The reader ends when the terminal's last writer closes it.
  let end = Instant::now() + Duration::from_secs(5);
  while !drain.is_finished() && Instant::now() < end {
    std::thread::sleep(Duration::from_millis(20));
  }
  (text(), status.exit_code())
}

/// The lines a program printed that start with `prefix`, without the
/// terminal's carriage returns. Typed input echoes after the prompt, so it
/// does not start a line.
fn lines_starting<'a>(output: &'a str, prefix: &str) -> Vec<&'a str> {
  output.split('\n').map(|line| line.trim_end_matches('\r')).filter(|line| line.starts_with(prefix)).collect()
}

#[test]
fn the_activation_script_defines_the_sandboxed_command_and_names_it_on_stderr() {
  let home = Home::new();
  let profile = home.profile("codex.yaml", "[/usr/local/bin/codex, --sandbox, danger-full-access]");
  let output = home.needy().args(["shell", "activate", "--print", "bash", "-f"]).arg(&profile).output().unwrap();
  assert!(output.status.success(), "{}", stderr(&output));
  assert!(stderr(&output).contains("needy: codex -> profile"), "{}", stderr(&output));
  let types = bash(r#"eval "$1"; type -t codex; type -t _needy_shell_deactivate"#, &[&stdout(&output)]);
  assert_eq!(stdout(&types), "function\nfunction\n", "{}", stderr(&types));
}

#[test]
fn the_needy_function_activates_and_deactivates_the_sandboxed_command() {
  let home = Home::new();
  // A name no host has, so that nothing answers to it once the sandboxed command is gone.
  home.profile("agent.yaml", "[stand-in-agent]");
  let output = home.shell_with_needy(
    "bash",
    r#"eval "$(needy shell init bash)"; PS1='$ '; needy shell activate -f agent.yaml; echo "on=$? kind=$(type -t stand-in-agent) [$PS1]"; needy --version >/dev/null; echo "other=$?"; needy shell deactivate; echo "off=$? kind=$(type -t stand-in-agent) [$PS1]""#,
  );
  assert_eq!(stdout(&output), "on=0 kind=function [(needy) $ ]\nother=0\noff=0 kind= [$ ]\n", "{}", stderr(&output));
  assert!(stderr(&output).contains("needy: stand-in-agent -> profile agent.yaml"), "{}", stderr(&output));
}

#[test]
fn the_needy_function_prints_help_instead_of_running_it() {
  let home = Home::new();
  let output = home.shell_with_needy("bash", r#"eval "$(needy shell init bash)"; needy shell activate --help; echo "status=$?""#);
  assert!(stdout(&output).contains("Usage: needy shell activate"), "{}", stdout(&output));
  assert!(stdout(&output).ends_with("status=0\n"), "{}", stdout(&output));
}

#[test]
fn deactivate_with_nothing_active_says_so() {
  let home = Home::new();
  let output = bash(r#"eval "$1"; needy shell deactivate; echo "status=$?""#, &[&home.init("bash")]);
  assert_eq!(stdout(&output), "status=1\n", "{}", stderr(&output));
  assert!(stderr(&output).contains("no sandboxed command is active"), "{}", stderr(&output));
}

#[test]
fn activate_without_the_needy_function_names_needy_shell_init() {
  let home = Home::new();
  let profile = home.profile("codex.yaml", "[codex]");
  let output = home.needy().args(["shell", "activate", "-f"]).arg(&profile).output().unwrap();
  assert_eq!(output.status.code(), Some(1));
  assert!(stdout(&output).is_empty());
  assert!(stderr(&output).contains(r#"eval "$(needy shell init zsh)""#), "{}", stderr(&output));
}

#[test]
fn init_on_a_terminal_prints_how_to_use_it_instead_of_the_code() {
  let home = Home::new();
  let argv = [env!("CARGO_BIN_EXE_needy"), "shell", "init", "zsh"].map(OsString::from);
  let (terminal, code) = on_terminal(&argv, &home.shell_env(), None, "");
  assert_eq!(code, 1, "{terminal}");
  assert!(terminal.contains(r#"eval "$(needy shell init zsh)""#), "{terminal}");
  assert!(!terminal.contains("function needy"), "{terminal}");
}

#[test]
fn a_call_runs_needy_run_create_with_the_callers_arguments_after_the_profile() {
  let home = Home::new();
  home.profile("codex.yaml", "[codex, --sandbox, danger-full-access]");
  // Relative paths become absolute: a call may come from another directory.
  let output = home
    .needy()
    .current_dir(home.path())
    .args([
      "--config",
      "cfg/config.yaml",
      "shell",
      "activate",
      "--print",
      "bash",
      "-f",
      "codex.yaml",
    ])
    .output()
    .unwrap();
  assert!(output.status.success(), "{}", stderr(&output));
  let bin = home.stand_in_needy();
  let args_file = home.path().join("args");
  let mut env = home.shell_env();
  let mut path = OsString::from(bin);
  path.push(":");
  path.push(std::env::var_os("PATH").unwrap_or_default());
  env.push(("PATH".into(), path));
  env.push(("NEEDY_ARGS_FILE".into(), args_file.clone().into()));
  let argv: Vec<OsString> = [
    "bash",
    "--norc",
    "--noprofile",
    "-c",
    r#"eval "$1"; cd /; codex a 'b c'"#,
    "bash",
    &stdout(&output),
  ]
  .map(OsString::from)
  .to_vec();
  let (terminal, code) = on_terminal(&argv, &env, None, "");
  assert_eq!(code, 0, "{terminal}");
  let root = std::fs::canonicalize(home.path()).unwrap();
  let config = root.join("cfg/config.yaml").display().to_string();
  let profile = root.join("codex.yaml").display().to_string();
  let recorded = std::fs::read_to_string(&args_file).unwrap();
  assert_eq!(
    recorded.lines().collect::<Vec<_>>(),
    [
      "--config", &config, "run", "create", "-it", "-f", &profile, "--", "a", "b c"
    ],
    "{terminal}"
  );
}

#[test]
fn a_named_or_builtin_profile_stays_a_name_in_the_call() {
  let home = Home::new();
  home.profile("config/profiles/codex.yaml", "[codex]");
  for (given, name) in [("codex", "codex"), ("builtin:opencode", "opencode")] {
    let script = home.needy().args(["shell", "activate", "--print", "bash", "-f", given]).output().unwrap();
    assert!(script.status.success(), "{given}: {}", stderr(&script));
    let call = format!("command needy run create -it -f {given} -- \"$@\"");
    assert!(stdout(&script).contains(&call), "{given}: {}", stdout(&script));
    assert!(stdout(&script).contains(&format!("function {name} {{")), "{given}: {}", stdout(&script));
  }
}

#[test]
fn a_call_without_a_terminal_is_refused_before_needy_runs() {
  let home = Home::new();
  let script = home.activation(&home.profile("codex.yaml", "[codex]"));
  let bin = home.stand_in_needy();
  let args_file = home.path().join("args");
  let output = Command::new("bash")
    .args([
      "--norc",
      "--noprofile",
      "-c",
      r#"eval "$1"; codex; echo "status=$?""#,
      "bash",
      &script,
    ])
    .env("PATH", format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default()))
    .env("NEEDY_ARGS_FILE", &args_file)
    .stdin(Stdio::null())
    .output()
    .unwrap();
  assert_eq!(stdout(&output), "status=1\n", "{}", stderr(&output));
  assert!(stderr(&output).contains("codex runs in a sandbox only from a terminal"), "{}", stderr(&output));
  assert!(!args_file.exists(), "needy ran without a terminal");
}

#[test]
fn an_alias_of_the_same_name_does_not_shadow_the_sandboxed_command() {
  let home = Home::new();
  let script = home.activation(&home.profile("codex.yaml", "[codex]"));
  let output = bash(r#"shopt -s expand_aliases; alias codex='echo alias'; eval "$1"; type -t codex"#, &[&script]);
  assert_eq!(stdout(&output), "function\n", "{}", stderr(&output));
}

#[test]
fn activating_another_profile_removes_the_previous_sandboxed_command() {
  let home = Home::new();
  let first = home.activation(&home.profile("first.yaml", "[stand-in-first]"));
  let second = home.activation(&home.profile("second.yaml", "[stand-in-second]"));
  let output = bash(
    r#"PS1='$ '; eval "$1"; eval "$2"; printf 'first=%s second=%s [%s]\n' "$(type -t stand-in-first)" "$(type -t stand-in-second)" "$PS1""#,
    &[&first, &second],
  );
  assert_eq!(stdout(&output), "first= second=function [(needy) $ ]\n", "{}", stderr(&output));
}

#[test]
fn the_needy_function_works_in_zsh() {
  if !installed("zsh") {
    return;
  }
  let home = Home::new();
  home.profile("agent.yaml", "[stand-in-agent]");
  let output = home.shell_with_needy(
    "zsh",
    r#"eval "$(needy shell init zsh)"; alias stand-in-agent='echo alias'; PS1='$ '; needy shell activate -f agent.yaml; print -r -- "kind=$(whence -w stand-in-agent) [$PS1]"; needy shell deactivate; print -r -- "after=$(whence -w stand-in-agent) [$PS1]""#,
  );
  assert_eq!(stdout(&output), "kind=stand-in-agent: function [(needy) $ ]\nafter=stand-in-agent: none [$ ]\n", "{}", stderr(&output));
}

#[test]
fn a_shell_as_the_driving_program_becomes_a_sandboxed_command() {
  let home = Home::new();
  let profile = home.profile("bash.yaml", "[bash]");
  let types = bash(r#"eval "$1"; type -t bash"#, &[&home.activation(&profile)]);
  assert_eq!(stdout(&types), "function\n", "{}", stderr(&types));
}

#[test]
fn needy_as_the_driving_program_is_refused() {
  let home = Home::new();
  let profile = home.profile("needy.yaml", "[needy, run, ls]");
  let output = home.needy().args(["shell", "activate", "--print", "bash", "-f"]).arg(&profile).output().unwrap();
  assert_eq!(output.status.code(), Some(1));
  assert!(stdout(&output).is_empty());
  assert!(stderr(&output).contains("The driving program `needy` is needy itself"), "{}", stderr(&output));
}

#[test]
fn the_subshell_names_the_supported_shells_for_another_one() {
  let home = Home::new();
  let profile = home.profile("codex.yaml", "[codex]");
  let output = home.needy().args(["shell", "--shell", "/usr/bin/fish", "-f"]).arg(&profile).output().unwrap();
  assert_eq!(output.status.code(), Some(1));
  assert!(stderr(&output).contains("needy shell supports zsh and bash"), "{}", stderr(&output));
}

#[test]
fn the_subshell_needs_a_terminal() {
  let home = Home::new();
  let profile = home.profile("codex.yaml", "[codex]");
  let output = home.needy().args(["shell", "--shell", "bash", "-f"]).arg(&profile).output().unwrap();
  assert_eq!(output.status.code(), Some(1));
  assert!(stderr(&output).contains("needs a terminal"), "{}", stderr(&output));
}

#[test]
fn the_zsh_subshell_runs_the_users_startup_files_first_and_leaves_no_files() {
  if !installed("zsh") {
    return;
  }
  let home = Home::new();
  let profile = home.profile("codex.yaml", "[codex]");
  home.write("h/.zshenv", "export FROM_ZSHENV=1\n");
  home.write("h/.zshrc", "alias codex='echo alias'\nPS1='$ '\n");
  let argv = [
    OsString::from(env!("CARGO_BIN_EXE_needy")),
    "shell".into(),
    "--shell".into(),
    "zsh".into(),
    "-f".into(),
    profile.into(),
  ];
  let input = concat!(
    r#"print -r -- "kind=$(whence -w codex) $(whence -w needy)"; "#,
    r#"print -r -- "state env=$FROM_ZSHENV zdotdir=${ZDOTDIR-unset} hist=$HISTFILE"; "#,
    r#"print -r -- "left=$(ls -A "$TMPDIR" | wc -l | tr -d ' ')"; exit 7"#,
    "\n"
  );
  let (terminal, code) = on_terminal(&argv, &home.shell_env(), Some("(needy) $ "), input);
  assert_eq!(code, 7, "{terminal}");
  assert!(terminal.contains("needy: codex -> profile"), "{terminal}");
  assert_eq!(lines_starting(&terminal, "kind="), ["kind=codex: function needy: function"], "{terminal}");
  let state = lines_starting(&terminal, "state ");
  assert_eq!(state.len(), 1, "{terminal}");
  assert!(state[0].starts_with("state env=1 zdotdir=unset hist="), "{terminal}");
  // The startup directory is in `$TMPDIR`; zsh's history must not be.
  let tmp = home.path().join("tmp").display().to_string();
  assert!(!state[0].contains(&tmp), "HISTFILE points into the startup directory:\n{terminal}");
  assert_eq!(lines_starting(&terminal, "left="), ["left=0"], "{terminal}");
}

#[test]
fn the_bash_subshell_runs_the_users_bashrc_first_and_leaves_no_files() {
  let home = Home::new();
  let profile = home.profile("codex.yaml", "[codex]");
  home.write("h/.bashrc", "alias codex='echo alias'\nPS1='$ '\n");
  let argv = [
    OsString::from(env!("CARGO_BIN_EXE_needy")),
    "shell".into(),
    "--shell".into(),
    "bash".into(),
    "-f".into(),
    profile.into(),
  ];
  let input =
    concat!(r#"echo "kind=$(type -t codex) $(type -t needy)"; "#, r#"echo "left=$(ls -A "$TMPDIR" | wc -l | tr -d ' ')"; exit 7"#, "\n");
  let (terminal, code) = on_terminal(&argv, &home.shell_env(), Some("(needy) $ "), input);
  assert_eq!(code, 7, "{terminal}");
  assert_eq!(lines_starting(&terminal, "kind="), ["kind=function function"], "{terminal}");
  assert_eq!(lines_starting(&terminal, "left="), ["left=0"], "{terminal}");
}
