use super::*;

fn route(text: &str) -> Route {
  Route::parse(text).unwrap()
}

fn strings(words: &[&str]) -> Vec<String> {
  words.iter().map(|w| w.to_string()).collect()
}

#[test]
fn a_route_file_reads_back_as_written() {
  let written = Route {
    uid: 1000,
    names: strings(&["python3", "curl"]),
  };

  assert_eq!(Route::parse(&written.render()), Some(written));
}

#[test]
fn a_listed_name_of_the_route_user_is_routed() {
  assert!(route("1000\npython3,curl\n").routes(1000, &strings(&["/usr/bin/python3", "python3"])));
}

#[test]
fn another_users_exec_is_never_routed() {
  // Init runs as root; its helpers stay in the actor, as the kernel's
  // `route_uid` keeps them in a VM.
  assert!(!route("1000\n*\n").routes(0, &strings(&["sh"])));
}

#[test]
fn a_star_routes_every_exec_of_the_route_user() {
  assert!(route("1000\n*\n").routes(1000, &strings(&["ls"])));
}

#[test]
fn an_unlisted_name_runs_its_real_program() {
  assert!(!route("1000\npython3\n").routes(1000, &strings(&["/usr/bin/ls", "ls"])));
}

#[test]
fn an_empty_route_routes_nothing() {
  assert!(!route("1000\n\n").routes(1000, &strings(&["ls"])));
}

#[test]
fn a_link_chain_is_matched_by_the_name_called_and_the_program_it_ends_at() {
  let names = names("/usr/bin/python3", Path::new("/usr/bin/python3.12"), "python3");

  assert_eq!(names, strings(&["/usr/bin/python3", "python3", "python3.12", "python3"]));
}

#[test]
fn a_script_is_matched_by_its_interpreter_as_the_shebang_names_it() {
  // `./build.sh` with `#!/bin/sh`: the kernel starts the `sh` shim, whose
  // file is dash, with `argv[0]` `/bin/sh`; `AT_EXECFN` is the script.
  let names = names("./build.sh", Path::new("/usr/bin/dash"), "/bin/sh");

  assert!(names.contains(&"sh".to_string()), "{names:?}");
  assert!(names.contains(&"build.sh".to_string()), "{names:?}");
}

#[test]
fn needys_own_files_are_never_shims() {
  assert_eq!(real(Path::new("/.needy/proxy")), None);
}
