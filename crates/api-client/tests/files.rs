//! Tar over exec streams, round trip against an in-process executor.

#![cfg(unix)]

mod support;

use needy_api_client::files::{self, Target};
use needy_api_client::{Error, Kind};
use needy_api_proto::v1::Origin;

fn target() -> Target {
  Target {
    executor: String::new(),
    run: String::new(),
    runner: String::new(),
    origin: Origin::Files,
  }
}

#[tokio::test]
async fn a_pushed_directory_pulls_back_the_same() {
  let executor = support::executor().await;
  let local = executor.dir.path().join("local");
  std::fs::create_dir_all(local.join("sub")).unwrap();
  std::fs::write(local.join("a.txt"), "alpha").unwrap();
  std::fs::write(local.join("sub/b.txt"), "beta").unwrap();
  let remote = executor.dir.path().join("remote");
  std::fs::create_dir_all(&remote).unwrap();
  let archive = needy_common::archive::pack(&local, &[]).unwrap();

  files::push(&executor.client, &target(), archive, remote.to_str().unwrap()).await.unwrap();
  let pulled = files::pull(&executor.client, &target(), remote.to_str().unwrap(), ".").await.unwrap();

  assert_eq!(std::fs::read_to_string(remote.join("sub/b.txt")).unwrap(), "beta");
  let back = executor.dir.path().join("back");
  needy_common::archive::unpack(&pulled, &back).unwrap();
  assert_eq!(std::fs::read_to_string(back.join("a.txt")).unwrap(), "alpha");
  assert_eq!(std::fs::read_to_string(back.join("sub/b.txt")).unwrap(), "beta");
}

// ROOT CAUSE:
//
// If the directory a push unpacks into did not exist on the executor, the
// push failed, because `tar -C` changes into the directory and does not
// create it.
//
// Before the fix, copy-in into a fresh native workspace and `needy fs cp`
// into a new directory failed until the directory was made by hand.
// The fix keeps a push creating its directory and the missing parents.
#[tokio::test]
async fn a_push_creates_its_missing_directory() {
  let executor = support::executor().await;
  let local = executor.dir.path().join("local");
  std::fs::create_dir_all(&local).unwrap();
  std::fs::write(local.join("a.txt"), "alpha").unwrap();
  let remote = executor.dir.path().join("new/nested");
  let archive = needy_common::archive::pack(&local, &[]).unwrap();

  files::push(&executor.client, &target(), archive, remote.to_str().unwrap()).await.unwrap();

  assert_eq!(std::fs::read_to_string(remote.join("a.txt")).unwrap(), "alpha");
}

#[tokio::test]
async fn a_push_into_a_path_that_is_a_file_fails() {
  let executor = support::executor().await;
  let file = executor.dir.path().join("file");
  std::fs::write(&file, "").unwrap();
  let archive = needy_common::archive::pack(executor.dir.path(), &["- **".to_string()]).unwrap();

  let error = files::push(&executor.client, &target(), archive, file.to_str().unwrap()).await.unwrap_err();

  assert!(matches!(error, Error::Failed { .. }), "{error:?}");
  assert!(error.to_string().contains("file"), "{error}");
}

#[tokio::test]
async fn a_pull_names_one_entry_of_a_directory() {
  let executor = support::executor().await;
  let remote = executor.dir.path().join("remote");
  std::fs::create_dir_all(remote.join("keep")).unwrap();
  std::fs::write(remote.join("keep/k.txt"), "k").unwrap();
  std::fs::write(remote.join("other.txt"), "o").unwrap();

  let pulled = files::pull(&executor.client, &target(), remote.to_str().unwrap(), "keep").await.unwrap();

  let back = executor.dir.path().join("back");
  needy_common::archive::unpack(&pulled, &back).unwrap();
  assert_eq!(std::fs::read_to_string(back.join("keep/k.txt")).unwrap(), "k");
  assert!(!back.join("other.txt").exists());
}

#[tokio::test]
async fn a_failed_tar_reports_its_error_output() {
  let executor = support::executor().await;
  let missing = executor.dir.path().join("missing");

  let error = files::pull(&executor.client, &target(), missing.to_str().unwrap(), ".").await.unwrap_err();

  assert!(matches!(error, Error::Failed { .. }), "{error:?}");
  assert_eq!(error.kind(), Kind::Failed);
  assert!(error.to_string().contains("missing"), "{error}");
}
