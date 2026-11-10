use serde_json::value::to_raw_value;

use super::*;

fn event(run: &str, seq: u64, time_ms: i64, kind: &str, payload: serde_json::Value) -> Event {
  Event {
    run: run.into(),
    seq,
    time_ms,
    kind: kind.into(),
    version: 1,
    payload: to_raw_value(&payload).unwrap(),
  }
}

#[tokio::test]
async fn written_envelopes_read_back_in_order() {
  let dir = tempfile::tempdir().unwrap();
  let path = dir.path().join("trace.db");
  let store = Store::open(&path).await.unwrap();
  let mut sink = store.sink();
  sink.write(&event("abc1", 0, 10, "run.create", serde_json::json!({"name": "demo"}))).await.unwrap();
  sink.write(&event("abc1", 1, 12, "trace.gap", serde_json::json!({"dropped": 4}))).await.unwrap();
  sink.write(&event("abc1", 2, 11, "run.stop", serde_json::json!({}))).await.unwrap();

  let loaded = load(&path, "abc").await.unwrap();

  assert_eq!(loaded.run, "abc1");
  let read: Vec<_> = loaded.events.iter().map(|e| (e.run.as_str(), e.seq, e.time_ms, e.kind.as_str(), e.version, e.payload.get())).collect();
  assert_eq!(
    read,
    [
      ("abc1", 0, 10, "run.create", 1, r#"{"name":"demo"}"#),
      ("abc1", 1, 12, "trace.gap", 1, r#"{"dropped":4}"#),
      ("abc1", 2, 11, "run.stop", 1, "{}"),
    ]
  );
}

#[tokio::test]
async fn latest_is_the_run_that_started_last() {
  let dir = tempfile::tempdir().unwrap();
  let path = dir.path().join("trace.db");
  let store = Store::open(&path).await.unwrap();
  let mut first = store.sink();
  let mut second = store.sink();
  first.write(&event("aaa", 0, 10, "run.create", serde_json::json!({}))).await.unwrap();
  second.write(&event("bbb", 0, 20, "run.create", serde_json::json!({}))).await.unwrap();
  // An event of the older run after the newer one started.
  first.write(&event("aaa", 1, 30, "run.stop", serde_json::json!({}))).await.unwrap();

  assert_eq!(load(&path, "latest").await.unwrap().run, "bbb");
  let older = load(&path, "a").await.unwrap();
  assert_eq!((older.run.as_str(), older.events.len()), ("aaa", 2));
}

#[tokio::test]
async fn reading_names_what_is_missing() {
  let dir = tempfile::tempdir().unwrap();
  let path = dir.path().join("trace.db");

  assert!(matches!(load(&path, "latest").await, Err(Error::NoDatabase(p)) if p == path));
  Store::open(&path).await.unwrap();
  assert!(matches!(load(&path, "zzz").await, Err(Error::NoRun(s)) if s == "zzz"));
}

// ROOT CAUSE:
//
// If the daemon opened a `trace.db` written before *session* became *run*,
// it would find its migration applied and fail on the missing `runs` table,
// because the renamed tables kept the old migration's name.
//
// The fix names the renamed schema's migration anew, so such a database is
// refused with the message that says to move it away.
#[tokio::test]
async fn a_database_from_before_the_run_rename_is_refused() {
  use sea_orm::{ConnectionTrait, Database};
  let dir = tempfile::tempdir().unwrap();
  let path = dir.path().join("trace.db");
  let old = Database::connect(format!("sqlite://{}?mode=rwc", path.display())).await.unwrap();
  old
    .execute_unprepared(
      "CREATE TABLE seaql_migrations (version TEXT PRIMARY KEY, applied_at BIGINT NOT NULL);
       INSERT INTO seaql_migrations VALUES ('m20261109_000010_trace_envelope', 0);
       CREATE TABLE sessions (id TEXT PRIMARY KEY, first_ms BIGINT NOT NULL);",
    )
    .await
    .unwrap();
  old.close().await.unwrap();

  let error = Store::open(&path).await.err().unwrap();

  assert!(matches!(&error, Error::Schema(p) if *p == path), "{error}");
}

// Principle 7 of the module layout redesign: trace databases from before the
// trace crate are not migrated. Opening one fails with a message that says
// so instead of a bare migration error.
#[tokio::test]
async fn a_database_with_another_schema_is_refused() {
  use sea_orm::{ConnectionTrait, Database};
  let dir = tempfile::tempdir().unwrap();
  let path = dir.path().join("trace.db");
  let old = Database::connect(format!("sqlite://{}?mode=rwc", path.display())).await.unwrap();
  old
    .execute_unprepared(
      "CREATE TABLE seaql_migrations (version TEXT PRIMARY KEY, applied_at BIGINT NOT NULL);
       INSERT INTO seaql_migrations VALUES ('m20261109_000002_typed_payload', 0);",
    )
    .await
    .unwrap();
  old.close().await.unwrap();

  let error = Store::open(&path).await.err().unwrap();

  assert!(matches!(&error, Error::Schema(p) if *p == path), "{error}");
  assert!(error.to_string().contains("move it away"), "{error}");
}
