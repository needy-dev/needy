use std::path::Path;

use super::{Shell, program_name};

fn words(command: &[&str]) -> Vec<String> {
  command.iter().map(|word| word.to_string()).collect()
}

#[test]
fn the_sandboxed_command_is_named_after_the_driving_program_s_file_name() {
  assert_eq!(program_name(&words(&["codex", "--sandbox", "danger-full-access"])).unwrap(), "codex");
  assert_eq!(program_name(&words(&["/usr/local/bin/opencode"])).unwrap(), "opencode");
  assert_eq!(program_name(&words(&["g++"])).unwrap(), "g++");
}

#[test]
fn a_profile_without_a_driving_program_has_no_sandboxed_command() {
  let error = program_name(&[]).unwrap_err();
  assert!(format!("{error:#}").contains("no driving program"), "{error:#}");
}

#[test]
fn a_shell_or_a_wrapper_as_the_driving_program_is_a_sandboxed_command() {
  assert_eq!(program_name(&words(&["bash"])).unwrap(), "bash");
  assert_eq!(program_name(&words(&["/bin/sh", "-c", "codex"])).unwrap(), "sh");
  assert_eq!(program_name(&words(&["/usr/bin/env", "codex"])).unwrap(), "env");
}

#[test]
fn needy_or_a_name_the_script_needs_is_refused() {
  for command in [
    &["needy"][..],
    &["/usr/local/bin/needy", "run", "ls"],
    &["command"],
    &["time", "codex"],
  ] {
    let error = program_name(&words(command)).unwrap_err();
    assert!(format!("{error:#}").contains("a builtin or reserved word that needy shell needs"), "{command:?}: {error:#}");
  }
}

#[test]
fn a_name_a_shell_function_cannot_take_is_refused() {
  for program in ["-codex", "my program", "co$dex", ".."] {
    let error = program_name(&words(&[program])).unwrap_err();
    assert!(format!("{error:#}").contains("cannot name a shell function"), "{program}: {error:#}");
  }
}

#[test]
fn the_shell_is_known_by_its_file_name() {
  assert_eq!(Shell::of(Path::new("/bin/zsh")), Some(Shell::Zsh));
  assert_eq!(Shell::of(Path::new("/opt/homebrew/bin/bash")), Some(Shell::Bash));
  assert_eq!(Shell::of(Path::new("-zsh")), Some(Shell::Zsh));
  assert_eq!(Shell::of(Path::new("/usr/bin/fish")), None);
}
