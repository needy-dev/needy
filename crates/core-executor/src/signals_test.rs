use super::*;

#[test]
fn names_and_numbers() {
  assert_eq!(number("SIGTERM"), Some(15));
  assert_eq!(name(9), "SIGKILL");
  assert_eq!(parse("TERM").as_deref(), Some("SIGTERM"));
  assert_eq!(parse("2").as_deref(), Some("SIGINT"));
  assert_eq!(parse("NOPE"), None);
  let exit = Exit {
    code: None,
    signal: "SIGTERM".into(),
  };
  assert_eq!(shell_code(&exit), 143);
}

#[cfg(unix)]
#[test]
fn platform_numbers_differ() {
  // SIGUSR1 is 10 on Linux and 30 on macOS; names are what travel.
  let usr1 = number("SIGUSR1").unwrap();
  assert_eq!(name(usr1), "SIGUSR1");
}
