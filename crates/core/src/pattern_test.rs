use super::*;

fn argv(s: &str) -> Vec<String> {
  s.split_whitespace().map(str::to_string).collect()
}

fn matches(pattern: &str, command: &str) -> Option<Match> {
  Pattern::compile(pattern).matches(&argv(command))
}

#[test]
fn words_match_like_the_command() {
  assert!(matches("git **", "git").is_some());
  assert!(matches("git **", "/usr/bin/git status -s").is_some());
  assert!(matches("git commit *", "git commit -a").is_some());
  assert!(matches("git commit *", "git commit -a -m").is_none());
  assert!(matches("pnpm dlx prettier **", "pnpm dlx prettier --write x").is_some());
  assert!(matches("pnpm dlx prettier **", "pnpm dlx pi").is_none());
  assert!(matches("**", "anything at all").is_some());
  assert!(matches("*.sh", "./build.sh").is_some());
}

#[test]
fn a_path_matches_the_resolved_executable_and_a_word_the_basename() {
  assert!(matches("/usr/local/bin/python3 **", "/usr/local/bin/python3 -V").is_some());
  assert!(matches("/usr/local/bin/python3 **", "python3 -V").is_none());
  assert!(matches("curl *", "/usr/bin/curl https://x/y").is_some());
}

#[test]
fn alternatives_captures_and_rest() {
  let m = matches("{node,bun} {script} **", "bun /opt/pi/dist/cli.js --x").unwrap();
  assert_eq!(m.captures["script"], "/opt/pi/dist/cli.js");
  assert_eq!(m.rest, argv("--x"));
  assert!(matches("{git,hg} push **", "git push origin main").is_some());
  assert!(matches("{git,hg} push **", "svn push").is_none());
}

#[test]
fn expand_substitutes_captures_and_the_rest() {
  let m = matches("mytool run {script} **", "mytool run deploy.sh --prod -v").unwrap();
  assert_eq!(m.expand(&argv("{script} --log {script}.log **")), argv("deploy.sh --log deploy.sh.log --prod -v"));
}

#[test]
fn command_names_are_literal_first_words() {
  assert_eq!(Pattern::compile("git **").command_names(), Some(vec!["git".to_string()]));
  assert_eq!(Pattern::compile("{git,make} **").command_names(), Some(vec!["git".to_string(), "make".to_string()]));
  assert_eq!(Pattern::compile("**").command_names(), None);
  assert_eq!(Pattern::compile("py* **").command_names(), None);
  assert_eq!(Pattern::compile("{script} **").command_names(), None);
}

#[test]
fn basename_is_the_last_path_component() {
  assert_eq!(basename("/usr/bin/git"), "git");
  assert_eq!(basename("git"), "git");
  assert_eq!(basename("./build.sh"), "build.sh");
}
