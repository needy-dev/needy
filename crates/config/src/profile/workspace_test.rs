use crate::Source;
use crate::profile::{FileMode, Profile, Side, load};

fn load_text(yaml: &str) -> Result<Profile, crate::Error> {
  load(&Source::text(yaml, "/base"))
}

fn error(yaml: &str) -> String {
  load_text(yaml).unwrap_err().to_string()
}

const EXECUTOR: &str = r#"
user: { name: dev }
actor:
  driver: linux-firecracker
  workspace: { path: ~/project, from: "executor:build:/srv/project/", writable: true }
executors:
  build: { driver: native }
"#;

#[test]
fn an_executor_workspace_derives_its_mount_and_path_map() {
  let p = load_text(EXECUTOR).unwrap();

  let mount = &p.files.mappings["workspace"];
  assert_eq!(mount.mode, FileMode::Mount);
  assert_eq!(
    mount.from,
    Side::Executor {
      name: "build".into(),
      path: "/srv/project".into()
    }
  );
  assert_eq!(mount.to, Side::Actor("/home/dev/project".into()));
  assert!(mount.writable);
  assert_eq!(p.executors["build"].path_map["/home/dev/project"], "/srv/project");
  assert_eq!(p.actor.workspace.as_ref().unwrap().path, "/home/dev/project");
}

#[test]
fn a_native_executor_without_a_workspace_starts_commands_in_the_workspace() {
  let p = load_text(EXECUTOR).unwrap();
  assert_eq!(p.executors["build"].workspace.as_deref(), Some("/srv/project"));

  let p = load_text(&EXECUTOR.replace("{ driver: native }", "{ driver: native, workspace: /elsewhere }")).unwrap();
  assert_eq!(p.executors["build"].workspace.as_deref(), Some("/elsewhere"));
}

// A `linux-container` executor's `workspace` is the host directory its
// container mounts, not a path in the container.
#[test]
fn a_container_executors_workspace_is_left_alone() {
  let p = load_text(&EXECUTOR.replace("{ driver: native }", "{ driver: linux-container }")).unwrap();
  assert_eq!(p.executors["build"].workspace, None);
}

// `~` on the executor's side is the executor's home, which only the daemon
// knows once the instance exists, so it stays as written.
#[test]
fn the_executor_path_keeps_its_tilde() {
  let p = load_text(&EXECUTOR.replace("/srv/project/", "~/project")).unwrap();
  assert_eq!(p.executors["build"].path_map["/home/dev/project"], "~/project");
  assert_eq!(
    p.files.mappings["workspace"].from,
    Side::Executor {
      name: "build".into(),
      path: "~/project".into()
    }
  );
}

#[test]
fn the_workspace_is_selected_even_when_run_files_does_not_name_it() {
  let p =
    load_text(&format!("{EXECUTOR}files:\n  seed: {{ mode: copy-in, from: \"host:/s\", to: \"actor:~/s\" }}\nrun_files: [seed]\n")).unwrap();
  assert_eq!(p.run_files, ["seed", "workspace"]);

  // An empty list already selects every mapping.
  assert!(load_text(EXECUTOR).unwrap().run_files.is_empty());
}

#[test]
fn a_host_workspace_is_copied_in_and_mirrored_out_when_writable() {
  let yaml = "user: { name: dev }\nactor: { driver: linux-firecracker, workspace: { path: ~/w, from: \"host:/src/w\", writable: true } }\n";
  let p = load_text(yaml).unwrap();

  let copy = &p.files.mappings["workspace"];
  assert_eq!((copy.mode, &copy.from, &copy.to), (FileMode::CopyIn, &Side::Host("/src/w".into()), &Side::Actor("/home/dev/w".into())));
  let out = &p.files.mappings["workspace-out"];
  assert_eq!((out.mode, &out.from, &out.to), (FileMode::MirrorOut, &Side::Actor("/home/dev/w".into()), &Side::Host("/src/w".into())));
  assert!(!out.delete);

  let p = load_text(&yaml.replace(", writable: true", "")).unwrap();
  assert!(!p.files.mappings.contains_key("workspace-out"));
}

#[test]
fn a_relative_host_workspace_resolves_against_the_profile() {
  let dir = tempfile::tempdir().unwrap();
  let file = dir.path().join("p.yaml");
  std::fs::write(&file, "actor: { driver: linux-firecracker, workspace: { path: ~/w, from: \"host:./src\" } }\n").unwrap();

  let resolved = crate::profile::resolve(&Source::File(file)).unwrap();
  let p = load(&Source::text(resolved.yaml, "/elsewhere")).unwrap();

  let absolute = std::path::absolute(dir.path()).unwrap();
  assert_eq!(p.files.mappings["workspace"].from, Side::Host(format!("{}/src", absolute.display())));
}

#[test]
fn a_workspace_without_from_derives_no_mapping() {
  let p = load_text("actor: { driver: linux-firecracker, workspace: { path: ~/scratch } }\n").unwrap();
  assert!(p.files.mappings.is_empty());
  assert_eq!(p.actor.workspace.unwrap().path, "/home/agent/scratch");
}

#[test]
fn a_workspace_outside_the_home_needs_an_executor() {
  for from in ["", ", from: \"host:/src\""] {
    let err = error(&format!("actor: {{ driver: linux-firecracker, workspace: {{ path: /workspace{from} }} }}\n"));
    assert!(err.contains("actor.workspace.path") && err.contains("outside the user's home"), "{err}");
  }
  let p = load_text(&EXECUTOR.replace("~/project", "/workspace")).unwrap();
  assert_eq!(p.executors["build"].path_map["/workspace"], "/srv/project");
}

#[test]
fn a_relative_workspace_path_is_refused() {
  let err = error("actor: { driver: linux-firecracker, workspace: { path: work } }\n");
  assert!(err.contains("actor.workspace.path") && err.contains("absolute"), "{err}");
}

#[test]
fn a_workspace_from_the_actor_is_refused() {
  let err = error("actor: { driver: linux-firecracker, workspace: { path: ~/w, from: \"actor:~/x\" } }\n");
  assert!(err.contains("actor.workspace.from"), "{err}");
}

#[test]
fn a_workspace_from_an_unknown_executor_is_refused() {
  let err = error(&EXECUTOR.replace("executor:build:", "executor:nope:"));
  assert!(err.contains("actor.workspace.from") && err.contains("unknown executor \"nope\""), "{err}");
}

#[test]
fn a_path_map_entry_for_the_workspace_is_refused_naming_both() {
  let err = error(&EXECUTOR.replace("{ driver: native }", "{ driver: native, path_map: { ~/project: /other } }"));
  assert!(err.contains("executors.build.path_map") && err.contains("/other") && err.contains("/srv/project"), "{err}");
}

#[test]
fn a_path_map_for_another_prefix_is_kept() {
  let p = load_text(&EXECUTOR.replace("{ driver: native }", "{ driver: native, path_map: { /opt/tools: /usr/local/tools } }")).unwrap();
  assert_eq!(p.executors["build"].path_map.len(), 2);
  assert_eq!(p.executors["build"].path_map["/opt/tools"], "/usr/local/tools");
}

#[test]
fn a_files_workspace_mapping_beside_actor_workspace_is_refused() {
  let err = error(&format!("{EXECUTOR}files:\n  workspace: {{ mode: copy-in, from: \"host:/s\", to: \"actor:~/s\" }}\n"));
  assert!(err.contains("files.workspace") && err.contains("actor.workspace"), "{err}");
}

#[test]
fn actor_cwd_is_an_unknown_field() {
  let err = error("actor: { driver: linux-firecracker, cwd: /tmp }\n");
  assert!(err.contains("actor") && err.contains("unknown field `cwd`"), "{err}");
}
