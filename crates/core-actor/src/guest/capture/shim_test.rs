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

/// A system `PATH` in `dir`: the stub at `.needy/proxy`, and the programs
/// `files` (path, content, mode) and links `links` (path, target) under it.
fn system(dir: &Path, files: &[(&str, &str, u32)], links: &[(&str, &str)]) -> PathBuf {
  use std::os::unix::fs::PermissionsExt;
  let root = std::fs::canonicalize(dir).unwrap();
  for (path, content, mode) in [(".needy/proxy", "stub", 0o755)].iter().chain(files) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, content).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(*mode)).unwrap();
  }
  for (path, target) in links {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(target, path).unwrap();
  }
  root
}

fn dirs(root: &Path, dirs: &[&str]) -> Vec<PathBuf> {
  dirs.iter().map(|d| root.join(d)).collect()
}

fn same_file(a: &Path, b: &Path) -> bool {
  use std::os::unix::fs::MetadataExt;
  let (a, b) = (std::fs::metadata(a).unwrap(), std::fs::metadata(b).unwrap());
  (a.dev(), a.ino()) == (b.dev(), b.ino())
}

// ROOT CAUSE:
//
// If a program found its files from its own path (Node.js's npm, its global
// prefix), it found `/.needy/real/...` in a container actor, because every
// program on `PATH` was a shim whose real program ran from `/.needy/real`.
//
// Before the fix, the guest needy ran every real program from `/.needy/real`.
// The fix reaches programs other than the in-place shells through a
// trampoline on `PATH`, whose real program runs at its own path.
#[test]
fn a_trampolines_real_program_is_at_its_own_path() {
  let dir = tempfile::tempdir().unwrap();
  let root = system(dir.path(), &[("usr/local/bin/node", "node", 0o755)], &[]);
  let (trampolines, search, stub) = (root.join("run/shims"), dirs(&root, &["usr/local/bin"]), root.join(".needy/proxy"));

  let made = trampolines_in(&trampolines, &search, &stub).unwrap();

  assert_eq!(made, 1);
  assert!(same_file(&trampolines.join("node"), &trampolines.join(TRAMPOLINE_STUB)), "node is not a link to the stub's copy");
  assert_eq!(std::fs::read_to_string(trampolines.join("node")).unwrap(), "stub");
  assert_eq!(behind_trampoline(&trampolines.join("node"), &search, &stub), Some(root.join("usr/local/bin/node")));
}

#[test]
fn the_first_program_of_a_name_in_path_order_is_behind_its_trampoline() {
  let dir = tempfile::tempdir().unwrap();
  let root = system(
    dir.path(),
    &[
      ("usr/local/bin/tool", "local", 0o755),
      ("usr/bin/tool", "system", 0o755),
    ],
    &[],
  );
  let (trampolines, search, stub) = (root.join("run/shims"), dirs(&root, &["usr/local/bin", "usr/bin"]), root.join(".needy/proxy"));

  assert_eq!(trampolines_in(&trampolines, &search, &stub).unwrap(), 1);

  assert_eq!(behind_trampoline(&trampolines.join("tool"), &search, &stub), Some(root.join("usr/local/bin/tool")));
}

// An image built by an earlier shim step has every program in place, and a
// shell always is: the lookup must reach that shim, which catches calls by
// its absolute path too, so no trampoline may stand in front of it.
#[test]
fn an_in_place_shim_gets_no_trampoline() {
  let dir = tempfile::tempdir().unwrap();
  let root = system(
    dir.path(),
    &[
      ("usr/local/bin/bash", "stub", 0o755),
      ("usr/bin/bash", "bash", 0o755),
    ],
    &[],
  );
  let (trampolines, search, stub) = (root.join("run/shims"), dirs(&root, &["usr/local/bin", "usr/bin"]), root.join(".needy/proxy"));

  assert_eq!(trampolines_in(&trampolines, &search, &stub).unwrap(), 0);

  assert!(!trampolines.join("bash").exists());
}

#[test]
fn a_file_that_is_not_a_program_gets_no_trampoline() {
  let dir = tempfile::tempdir().unwrap();
  let root = system(
    dir.path(),
    &[
      ("usr/bin/notes", "data", 0o644),
      ("usr/bin/.hidden", "x", 0o755),
    ],
    &[("usr/bin/gone", "nowhere")],
  );
  let (trampolines, search, stub) = (root.join("run/shims"), dirs(&root, &["usr/bin"]), root.join(".needy/proxy"));

  assert_eq!(trampolines_in(&trampolines, &search, &stub).unwrap(), 0);
}

#[test]
fn a_link_on_path_gets_a_trampoline_whose_real_program_is_the_link() {
  // `python3` -> `python3.12`: the link's own path runs, so its link chain,
  // and the program it ends at, are the caller's.
  let dir = tempfile::tempdir().unwrap();
  let root = system(dir.path(), &[("usr/bin/python3.12", "py", 0o755)], &[("usr/bin/python3", "python3.12")]);
  let (trampolines, search, stub) = (root.join("run/shims"), dirs(&root, &["usr/bin"]), root.join(".needy/proxy"));

  assert_eq!(trampolines_in(&trampolines, &search, &stub).unwrap(), 2);

  assert_eq!(behind_trampoline(&trampolines.join("python3"), &search, &stub), Some(root.join("usr/bin/python3")));
}

#[test]
fn a_directory_reached_through_a_link_is_searched_once() {
  let dir = tempfile::tempdir().unwrap();
  let root = system(dir.path(), &[("usr/bin/tool", "tool", 0o755)], &[("bin", "usr/bin")]);
  let (trampolines, search, stub) = (root.join("run/shims"), dirs(&root, &["usr/bin", "bin"]), root.join(".needy/proxy"));

  assert_eq!(trampolines_in(&trampolines, &search, &stub).unwrap(), 1);
}

#[test]
fn making_the_trampolines_again_keeps_them() {
  let dir = tempfile::tempdir().unwrap();
  let root = system(dir.path(), &[("usr/bin/tool", "tool", 0o755)], &[]);
  let (trampolines, search, stub) = (root.join("run/shims"), dirs(&root, &["usr/bin"]), root.join(".needy/proxy"));
  trampolines_in(&trampolines, &search, &stub).unwrap();

  assert_eq!(trampolines_in(&trampolines, &search, &stub).unwrap(), 0);
  assert!(same_file(&trampolines.join("tool"), &trampolines.join(TRAMPOLINE_STUB)));
}

#[test]
fn image_path_file_round_trips_and_falls_back_to_system_path() {
  let dir = tempfile::tempdir().unwrap();
  let file = dir.path().join("needy/path");

  assert_eq!(read_path_from(&file), SYSTEM_PATH);

  write_path_to(&file, "/opt/node/bin:/usr/bin").unwrap();
  assert_eq!(read_path_from(&file), "/opt/node/bin:/usr/bin");

  write_path_to(&file, "   ").unwrap();
  assert_eq!(read_path_from(&file), SYSTEM_PATH);
}
