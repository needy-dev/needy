use needy_api_server::control::{ControlError, Runs};
use needy_core::types::{Principal, RunState};

use crate::testing::{self, new_run};

fn profile(files: &str) -> String {
  format!(
    r#"
user: {{ name: agent, uid: 1000, gid: 1000 }}
actor: {{ driver: linux-firecracker }}
executors:
  here: {{ driver: native }}
commands:
  - {{ match: "**", to: here }}
files:
{files}
"#
  )
}

// ROOT CAUSE:
//
// If a run's `host:` copy-in named a path that did not exist on the control
// host, such as a path of the remote client that sent the profile, packing
// it found nothing and the run went on with nothing copied.
//
// Before the fix, the run was created and finished without the files.
// The fix checks every `host:` path the run uses on the control host before
// the run acquires anything, and fails the create naming the path and where
// it was looked for.
#[tokio::test]
async fn a_copy_in_whose_host_path_is_missing_on_the_control_host_fails_the_create() {
  let started = testing::daemon().await;
  let missing = started.dir.path().join("client-only").join("seed");
  let yaml = profile(&format!(r#"  seed: {{ mode: copy-in, from: "host:{}", to: "actor:~/seed/" }}"#, missing.display()));

  let created = Runs::create(&started.daemon, new_run(&yaml, started.dir.path())).await;

  let Err(ControlError::Failed(message)) = created else {
    panic!("a copy-in of a missing host path must fail the create, not {created:?}");
  };
  assert!(message.contains("copy-in seed"), "{message}");
  assert!(message.contains(&format!("host path {} does not exist on the control host", missing.display())), "{message}");
  let listed = Runs::list(&started.daemon).await.unwrap();
  assert_eq!(listed[0].state, RunState::Failed);
}

// ROOT CAUSE:
//
// If a run's `host:` mirror-out named a directory under one the control host
// does not have, such as a remote client's home directory, the control
// daemon created the whole tree, as the user it runs as.
//
// Before the fix, `/Users/...` appeared on a Linux control host.
// The fix creates a mirror-out destination only under a directory that
// exists on the control host, and refuses the run otherwise.
#[tokio::test]
async fn a_mirror_out_under_a_directory_the_control_host_does_not_have_fails_the_create() {
  let started = testing::daemon().await;
  let elsewhere = started.dir.path().join("client-home");
  let destination = elsewhere.join("out");
  let yaml = profile(&format!(r#"  results: {{ mode: mirror-out, from: "actor:~/out/", to: "host:{}" }}"#, destination.display()));

  let created = Runs::create(&started.daemon, new_run(&yaml, started.dir.path())).await;

  let Err(ControlError::Failed(message)) = created else {
    panic!("a mirror-out the control host cannot hold must fail the create, not {created:?}");
  };
  assert!(message.contains("mirror-out results"), "{message}");
  assert!(message.contains(&format!("{} does not exist on the control host", elsewhere.display())), "{message}");
  assert!(!elsewhere.exists(), "the refused mirror-out created its tree");
}

// ROOT CAUSE:
//
// If a profile listed `run_files`, mirror-out from an executor at the stop
// still unpacked every executor-to-host mapping on the host, because it
// skipped the `run_files` selection that copy-in, the host-path check and
// the actor's file list apply.
//
// Before the fix, a mapping the run did not select wrote the host.
// The fix mirrors out only the mappings the run selects, as copy-in does.
#[tokio::test]
async fn a_mirror_out_from_an_executor_the_run_files_do_not_name_does_not_run() {
  let started = testing::daemon().await;
  let source = started.dir.path().join("produced");
  std::fs::create_dir_all(&source).unwrap();
  std::fs::write(source.join("result.txt"), "done").unwrap();
  let chosen = started.dir.path().join("chosen");
  let other = started.dir.path().join("other");
  let mut yaml = profile(&format!(
    "  chosen: {{ mode: mirror-out, from: \"executor:here:{src}\", to: \"host:{chosen}\" }}\n  \
     other: {{ mode: mirror-out, from: \"executor:here:{src}\", to: \"host:{other}\" }}",
    src = source.display(),
    chosen = chosen.display(),
    other = other.display(),
  ));
  yaml.push_str("run_files: [chosen]\n");
  let created = Runs::create(&started.daemon, new_run(&yaml, started.dir.path())).await.unwrap();

  Runs::stop(&started.daemon, &created.id, Principal::Operator, needy_api_server::control::StopMode::default()).await.unwrap();

  assert_eq!(std::fs::read_to_string(chosen.join("result.txt")).unwrap(), "done");
  assert!(!other.exists(), "a mirror-out run_files does not name wrote the host");
  let trace = needy_trace::sink::sqlite::load(&started.dir.path().join("state/trace.db"), &created.id).await.unwrap();
  let mirrored: Vec<&str> = trace.events.iter().filter(|e| e.kind == "files.mirror_out").map(|e| e.payload.get()).collect();
  assert_eq!(mirrored.len(), 1, "{mirrored:?}");
  assert!(mirrored[0].contains(r#""name":"chosen""#), "{mirrored:?}");
}

#[tokio::test]
async fn a_mirror_out_that_fails_at_the_stop_is_recorded_for_the_report() {
  let started = testing::daemon().await;
  let source = started.dir.path().join("produced");
  std::fs::create_dir_all(&source).unwrap();
  std::fs::write(source.join("result.txt"), "done").unwrap();
  let anchor = started.dir.path().join("results");
  std::fs::create_dir_all(&anchor).unwrap();
  let yaml = profile(&format!(
    r#"  results: {{ mode: mirror-out, from: "executor:here:{}", to: "host:{}" }}"#,
    source.display(),
    anchor.join("out").display()
  ));
  let created = Runs::create(&started.daemon, new_run(&yaml, started.dir.path())).await.unwrap();
  // The directory the destination would go under is gone by the stop.
  std::fs::remove_dir_all(&anchor).unwrap();

  Runs::stop(&started.daemon, &created.id, Principal::Operator, needy_api_server::control::StopMode::default()).await.unwrap();

  assert!(!anchor.exists(), "the mirror-out created a tree the control host did not have");
  let trace = needy_trace::sink::sqlite::load(&started.dir.path().join("state/trace.db"), &created.id).await.unwrap();
  let failed = trace.events.iter().find(|e| e.kind == "files.fail").expect("a files.fail event");
  assert!(failed.payload.get().contains(r#""name":"results""#), "{}", failed.payload.get());
  assert!(failed.payload.get().contains("does not exist on the control host"), "{}", failed.payload.get());
}

/// A directory with `keep.txt`, `cache/blob` and `nested/debug.log`, for
/// a filter to pick from.
fn produced(dir: &std::path::Path) -> std::path::PathBuf {
  let source = dir.join("produced");
  std::fs::create_dir_all(source.join("cache")).unwrap();
  std::fs::create_dir_all(source.join("nested")).unwrap();
  std::fs::write(source.join("keep.txt"), "kept").unwrap();
  std::fs::write(source.join("cache/blob"), "x").unwrap();
  std::fs::write(source.join("nested/debug.log"), "x").unwrap();
  source
}

const FILTER: &str = r#"["- /cache/", "- *.log", "+ **"]"#;

/// Asserts that `out` has what [`FILTER`] keeps of [`produced`] and none of
/// what it leaves out.
fn assert_filtered(out: &std::path::Path) {
  assert_eq!(std::fs::read_to_string(out.join("keep.txt")).unwrap(), "kept");
  assert!(!out.join("cache").exists(), "an excluded directory reached the host");
  assert!(out.join("nested").is_dir());
  assert!(!out.join("nested/debug.log").exists(), "an excluded file reached the host");
}

// ROOT CAUSE:
//
// If an actor's mirror-out mapping had a `filter`, everything the guest
// sent still reached the host, because the guest packed the whole directory
// and the daemon unpacked the archive as it came.
//
// Before the fix, `cache/` and `*.log` were written to the host.
// The fix applies the mapping's filter on the daemon when it unpacks, so a
// guest cannot bypass it.
#[tokio::test]
async fn an_actor_mirror_out_applies_the_mappings_filter_on_the_host() {
  use needy_api_server::control::Guest;
  let started = testing::daemon().await;
  let source = produced(started.dir.path());
  let out = started.dir.path().join("out");
  let yaml = profile(&format!(r#"  results: {{ mode: mirror-out, from: "actor:~/out/", to: "host:{}", filter: {FILTER} }}"#, out.display()));
  let created = Runs::create(&started.daemon, new_run(&yaml, started.dir.path())).await.unwrap();
  // What a guest sends: the whole directory.
  let archive = needy_common::archive::pack(&source, &[]).unwrap();

  Guest::push_files(&started.daemon, &created.id, "results", archive).await.unwrap();

  assert_filtered(&out);
}

// ROOT CAUSE:
//
// If an executor's mirror-out mapping had a `filter`, the executor's `tar`
// packed the whole directory and the daemon unpacked all of it, as for an
// actor's.
//
// Before the fix, `cache/` and `*.log` were written to the host.
// The fix applies the filter where the daemon unpacks, for both sides.
#[tokio::test]
async fn an_executor_mirror_out_applies_the_mappings_filter_on_the_host() {
  let started = testing::daemon().await;
  let source = produced(started.dir.path());
  let out = started.dir.path().join("out");
  let yaml = profile(&format!(
    r#"  results: {{ mode: mirror-out, from: "executor:here:{}", to: "host:{}", filter: {FILTER} }}"#,
    source.display(),
    out.display()
  ));
  let created = Runs::create(&started.daemon, new_run(&yaml, started.dir.path())).await.unwrap();

  Runs::stop(&started.daemon, &created.id, Principal::Operator, needy_api_server::control::StopMode::default()).await.unwrap();

  assert_filtered(&out);
}

// ROOT CAUSE:
//
// If an actor completed naturally, saving its terminal state revoked the
// client holder before mirror-out opened its file stream. Cleanup still owns
// the executor until its instance reference is released.
#[tokio::test]
async fn a_completed_actor_mirrors_client_files_before_revoking_its_holder() {
  use std::time::Duration;

  use needy_api_server::control::{Credential, Runners, StopMode};
  use tokio_util::task::AbortOnDropHandle;

  let started = testing::daemon().await;
  testing::fake_image(&started.dir.path().join("image"), testing::REBOOTS_AFTER_EXIT);
  let served = testing::serve(started, None).await;
  let daemon = served.daemon();
  let source = served.dir().join("produced");
  std::fs::create_dir(&source).unwrap();
  std::fs::write(source.join("result.bin"), b"completed\0\xffbytes\n").unwrap();
  let destination = served.dir().join("mirrored");
  let reserved = Runs::prepare_client_execution(daemon, Default::default()).await.unwrap();
  let capability = reserved.capability.clone();
  let holder = reserved.holder.clone();
  let helper = AbortOnDropHandle::new(tokio::spawn(crate::runner::serve_client_execution(crate::runner::ClientExecutionConfig {
    control: Some(served.endpoint.clone()),
    command: None,
    headers: Default::default(),
    holder: holder.clone(),
    capability: capability.clone(),
    run: reserved.run,
    directory: served.dir().join("client-holder"),
  })));
  // Wait for this real transport during fixture setup; the assertions below
  // use the run, file artifact, and authentication domain boundaries.
  daemon.links.connected(&holder, Duration::from_secs(5)).await.expect("client holder connected");
  let yaml = format!(
    "actor: {{driver: linux-firecracker}}\nexecutors:\n  client: {{driver: native, runner: {{client: true}}}}\nfiles:\n  result: {{mode: mirror-out, from: \"executor:client:{}\", to: \"host:{}\"}}\n",
    source.display(),
    destination.display()
  );
  let mut request = new_run(&yaml, served.dir());
  request.client_execution = reserved.capability;
  let created = Runs::create(daemon, request).await.unwrap();
  testing::exit_actor(daemon, served.dir(), &created.id).await;
  let released = tokio::time::timeout(Duration::from_secs(10), async {
    loop {
      let info = Runs::get(daemon, &created.id).await.unwrap();
      if info.state == RunState::Finished
        && info.executors.is_empty()
        && matches!(Runners::credential(daemon, &capability).await.unwrap(), Credential::Refused)
      {
        break info;
      }
      tokio::time::sleep(Duration::from_millis(20)).await;
    }
  })
  .await;
  // Explicit cleanup also runs if completion failed, before asserting output.
  Runs::stop(daemon, &created.id, Principal::Operator, StopMode::Now).await.unwrap();
  let info = released.expect("natural completion released its executor");
  assert_eq!(info.released.len(), 1);
  assert!(
    matches!(Runners::credential(daemon, &capability).await.unwrap(), Credential::Refused),
    "released holder credential remains active"
  );
  tokio::time::timeout(Duration::from_secs(10), helper)
    .await
    .expect("released holder exits")
    .unwrap()
    .expect("normal invocation revocation is a successful holder exit");
  assert_eq!(std::fs::read(destination.join("result.bin")).expect("natural completion mirrored client output"), b"completed\0\xffbytes\n");
}

#[tokio::test]
async fn disable_host_files_rejects_copy_in_mirror_out_and_host_workspace() {
  let started = testing::daemon_with(|config| config.disable_host_files = true).await;
  let seed = started.dir.path().join("seed");
  std::fs::create_dir_all(&seed).unwrap();
  std::fs::write(seed.join("secret.txt"), "top-secret").unwrap();
  let out = started.dir.path().join("out");

  // Copy-in from host: rejected.
  let copy_in_yaml = profile(&format!(r#"  seed: {{ mode: copy-in, from: "host:{}", to: "actor:~/seed/" }}"#, seed.display()));
  let err = Runs::create(&started.daemon, new_run(&copy_in_yaml, started.dir.path())).await.unwrap_err();
  assert!(err.to_string().contains("host: file mappings are disabled"), "{err}");

  // Mirror-out to host: rejected.
  let mirror_out_yaml = profile(&format!(r#"  out: {{ mode: mirror-out, from: "actor:~/out/", to: "host:{}" }}"#, out.display()));
  let err = Runs::create(&started.daemon, new_run(&mirror_out_yaml, started.dir.path())).await.unwrap_err();
  assert!(err.to_string().contains("host: file mappings are disabled"), "{err}");

  // Host workspace (desugared into host: copy-in/mirror-out): rejected.
  let ws_yaml = format!(
    r#"
user: {{ name: agent, uid: 1000, gid: 1000 }}
actor:
  driver: linux-firecracker
  workspace: {{ path: ~/workspace, from: "host:{}" }}
executors:
  here: {{ driver: native }}
commands:
  - {{ match: "**", to: here }}
"#,
    seed.display()
  );
  let err = Runs::create(&started.daemon, new_run(&ws_yaml, started.dir.path())).await.unwrap_err();
  assert!(err.to_string().contains("host: file mappings are disabled"), "{err}");
}

#[tokio::test]
async fn host_files_root_allows_paths_inside_root_and_blocks_outside_and_symlink_escapes() {
  use std::os::unix::fs::symlink;

  let dir = tempfile::tempdir().unwrap();
  let allowed_root = dir.path().join("allowed");
  let outside = dir.path().join("outside");
  std::fs::create_dir_all(allowed_root.join("in")).unwrap();
  std::fs::create_dir_all(&outside).unwrap();
  std::fs::write(allowed_root.join("in/ok.txt"), "ok").unwrap();
  std::fs::write(outside.join("secret.txt"), "secret").unwrap();

  // Symlinks inside allowed_root pointing outside.
  let escape_read = allowed_root.join("escape-read");
  symlink(&outside, &escape_read).unwrap();
  let escape_write_dir = allowed_root.join("escape-write");
  symlink(&outside, &escape_write_dir).unwrap();

  let root_for_config = allowed_root.clone();
  let started = testing::daemon_in(dir, move |config| {
    config.host_files_root = Some(root_for_config);
  })
  .await;

  // 1. Copy-in and mirror-out inside allowed_root succeed.
  let ok_out = allowed_root.join("out");
  let ok_yaml = profile(&format!(
    "  seed: {{ mode: copy-in, from: \"host:{}\", to: \"actor:~/seed/\" }}\n  \
     results: {{ mode: mirror-out, from: \"actor:~/out/\", to: \"host:{}\" }}",
    allowed_root.join("in").display(),
    ok_out.display(),
  ));
  let created = Runs::create(&started.daemon, new_run(&ok_yaml, started.dir.path())).await.unwrap();
  Runs::stop(&started.daemon, &created.id, Principal::Operator, needy_api_server::control::StopMode::Now).await.unwrap();

  // 2. Direct path outside allowed_root is rejected.
  let outside_yaml = profile(&format!(r#"  seed: {{ mode: copy-in, from: "host:{}", to: "actor:~/seed/" }}"#, outside.display()));
  let err = Runs::create(&started.daemon, new_run(&outside_yaml, started.dir.path())).await.unwrap_err();
  assert!(err.to_string().contains("outside host-files-root"), "{err}");

  // 3. Symlink inside allowed_root escaping for copy-in is rejected.
  let symlink_read_yaml = profile(&format!(r#"  seed: {{ mode: copy-in, from: "host:{}", to: "actor:~/seed/" }}"#, escape_read.display()));
  let err = Runs::create(&started.daemon, new_run(&symlink_read_yaml, started.dir.path())).await.unwrap_err();
  assert!(err.to_string().contains("outside host-files-root"), "{err}");

  // 4. Symlink inside allowed_root escaping for mirror-out (existing dir) is rejected.
  let symlink_write_yaml =
    profile(&format!(r#"  results: {{ mode: mirror-out, from: "actor:~/out/", to: "host:{}" }}"#, escape_write_dir.display()));
  let err = Runs::create(&started.daemon, new_run(&symlink_write_yaml, started.dir.path())).await.unwrap_err();
  assert!(err.to_string().contains("outside host-files-root"), "{err}");

  // 5. Non-existent child under a symlink escaping allowed_root for mirror-out is rejected.
  let symlink_nested_yaml =
    profile(&format!(r#"  results: {{ mode: mirror-out, from: "actor:~/out/", to: "host:{}" }}"#, escape_write_dir.join("child").display()));
  let err = Runs::create(&started.daemon, new_run(&symlink_nested_yaml, started.dir.path())).await.unwrap_err();
  assert!(err.to_string().contains("outside host-files-root"), "{err}");

  // 6. Out-of-root non-existent path and out-of-root regular file must fail
  // with "outside host-files-root" BEFORE probing existence or opening.
  let outside_missing = outside.join("does-not-exist");
  let outside_missing_copy =
    profile(&format!(r#"  seed: {{ mode: copy-in, from: "host:{}", to: "actor:~/seed/" }}"#, outside_missing.display()));
  let err = Runs::create(&started.daemon, new_run(&outside_missing_copy, started.dir.path())).await.unwrap_err();
  assert!(err.to_string().contains("outside host-files-root"), "must not leak existence: {err}");

  let outside_file_mirror =
    profile(&format!(r#"  results: {{ mode: mirror-out, from: "actor:~/out/", to: "host:{}" }}"#, outside.join("secret.txt").display()));
  let err = Runs::create(&started.daemon, new_run(&outside_file_mirror, started.dir.path())).await.unwrap_err();
  assert!(err.to_string().contains("outside host-files-root"), "must not leak file type: {err}");
}
