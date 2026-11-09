//! Session traces: SQLite for queries, JSONL per session for export.
//!
//! Writes go through a channel so recording never blocks the operation being
//! recorded.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use sea_orm::sea_query::{Expr, Func, OnConflict};
use sea_orm::{ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::store::{self, event, session};

#[derive(Debug, Clone)]
pub struct Event {
  pub session: String,
  pub time_ms: i64,
  pub kind: String,
  pub attrs: BTreeMap<String, String>,
}

#[derive(Clone)]
pub struct Trace {
  tx: mpsc::UnboundedSender<Event>,
}

pub fn now_ms() -> i64 {
  SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

impl Trace {
  pub async fn open(db: &Path, sessions_dir: PathBuf) -> Result<Self> {
    let db = store::open(db).await?;
    let (tx, mut rx) = mpsc::unbounded_channel::<Event>();
    tokio::spawn(async move {
      while let Some(event) = rx.recv().await {
        if let Err(err) = write(&db, &sessions_dir, &event).await {
          tracing::warn!("trace {} for session {}: {err:#}", event.kind, event.session);
        }
      }
    });
    Ok(Self { tx })
  }

  pub fn record<I, K, V>(&self, session: &str, kind: &str, attrs: I)
  where
    I: IntoIterator<Item = (K, V)>,
    K: Into<String>,
    V: Into<String>,
  {
    self.record_event(Event {
      session: session.to_string(),
      time_ms: now_ms(),
      kind: kind.to_string(),
      attrs: attrs.into_iter().map(|(k, v)| (k.into(), v.into())).collect(),
    });
  }

  pub fn record_event(&self, event: Event) {
    // The writer only stops when the runtime shuts down, and then nothing is
    // left to record.
    let _ = self.tx.send(event);
  }
}

/// Stores one event, updates its session row and appends it to the session's
/// `trace.jsonl`.
async fn write(db: &DatabaseConnection, sessions_dir: &Path, event: &Event) -> Result<()> {
  let attrs = serde_json::to_string(&event.attrs)?;
  event::Entity::insert(event::ActiveModel {
    session: Set(event.session.clone()),
    time_ms: Set(event.time_ms),
    kind: Set(event.kind.clone()),
    attrs: Set(attrs),
    ..Default::default()
  })
  .exec(db)
  .await
  .context("insert event")?;
  match event.kind.as_str() {
    "session.create" => {
      let row = session::ActiveModel {
        id: Set(event.session.clone()),
        name: Set(event.attrs.get("name").cloned()),
        grp: Set(event.attrs.get("group").cloned()),
        profile: Set(event.attrs.get("profile").cloned()),
        created_ms: Set(Some(event.time_ms)),
        ended_ms: Set(None),
        state: Set(Some("running".into())),
      };
      let columns = [
        session::Column::Name,
        session::Column::Grp,
        session::Column::Profile,
        session::Column::CreatedMs,
        session::Column::EndedMs,
        session::Column::State,
      ];
      session::Entity::insert(row)
        .on_conflict(OnConflict::column(session::Column::Id).update_columns(columns).to_owned())
        .exec(db)
        .await
        .context("insert session")?;
    }
    "session.stop" | "actor.exit" => {
      let state = if event.kind == "session.stop" {
        "stopped"
      } else {
        "finished"
      };
      // The first end wins: a stop after the actor exited keeps the exit time.
      let ended = Func::coalesce([
        Expr::col(session::Column::EndedMs),
        Expr::val(event.time_ms),
      ]);
      session::Entity::update_many()
        .col_expr(session::Column::EndedMs, ended.into())
        .col_expr(session::Column::State, Expr::val(state))
        .filter(session::Column::Id.eq(&event.session))
        .exec(db)
        .await
        .context("update session")?;
    }
    _ => {}
  }
  // Only sessions with a directory (those started by the daemon) export JSONL.
  let dir = sessions_dir.join(&event.session);
  if !event.session.is_empty() && dir.is_dir() {
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("trace.jsonl")).context("open trace.jsonl")?;
    let line = json!({"time_ms": event.time_ms, "kind": event.kind, "attrs": event.attrs});
    writeln!(file, "{line}").context("append trace.jsonl")?;
  }
  Ok(())
}

/// Starts an asciicast v2 recording of a terminal session.
pub fn open_cast(dir: &Path, process: &str, cols: u32, rows: u32, argv: &[String]) -> Option<std::fs::File> {
  std::fs::create_dir_all(dir).ok()?;
  let mut file = std::fs::File::create(dir.join(format!("{process}.cast"))).ok()?;
  let header = json!({
      "version": 2,
      "width": cols,
      "height": rows,
      "timestamp": now_ms() / 1000,
      "command": argv.join(" "),
  });
  writeln!(file, "{header}").ok()?;
  Some(file)
}

/// Appends terminal output to an asciicast recording.
pub fn cast_output(file: &mut std::fs::File, started_ms: i64, bytes: &[u8]) {
  let seconds = (now_ms() - started_ms) as f64 / 1000.0;
  let event = json!([seconds, "o", String::from_utf8_lossy(bytes)]);
  // A failed write loses part of a recording, never the command's output.
  // TODO(trace): terminal recordings are removed in slice 4.
  let _ = writeln!(file, "{event}");
}

pub struct Loaded {
  pub session: BTreeMap<String, Value>,
  pub events: Vec<(i64, String, BTreeMap<String, String>)>,
}

/// Reads one session, or the latest one, for reports. `session` may be an
/// id prefix.
pub async fn load(db: &Path, session: &str) -> Result<Loaded> {
  anyhow::ensure!(db.exists(), "no trace database at {}; start a daemon first", db.display());
  let db = store::open(db).await?;
  let mut query = session::Entity::find().order_by_desc(session::Column::CreatedMs);
  if session != "latest" {
    query = query.filter(session::Column::Id.starts_with(session));
  }
  let row = query.one(&db).await?.with_context(|| format!("no session matches {session}"))?;
  let events = event::Entity::find()
    .filter(event::Column::Session.eq(&row.id))
    .order_by_asc(event::Column::Id)
    .all(&db)
    .await?
    .into_iter()
    .map(|e| (e.time_ms, e.kind, serde_json::from_str(&e.attrs).unwrap_or_default()))
    .collect();
  let info = BTreeMap::from([
    ("id".to_string(), json!(row.id)),
    ("name".to_string(), json!(row.name)),
    ("group".to_string(), json!(row.grp)),
    ("profile".to_string(), json!(row.profile)),
    ("created_ms".to_string(), json!(row.created_ms)),
    ("ended_ms".to_string(), json!(row.ended_ms)),
    ("state".to_string(), json!(row.state)),
  ]);
  Ok(Loaded {
    session: info,
    events,
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use sea_orm::{ConnectionTrait, Database};

  fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("needy-trace-{}-{name}", std::process::id()));
    // A directory left by an earlier run may or may not exist.
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("sessions/s1")).unwrap();
    dir
  }

  fn event(kind: &str, time_ms: i64, attrs: &[(&str, &str)]) -> Event {
    Event {
      session: "s1".into(),
      time_ms,
      kind: kind.into(),
      attrs: attrs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
    }
  }

  #[tokio::test]
  async fn records_session_lifecycle_and_loads_by_prefix() {
    let dir = scratch("lifecycle");
    let path = dir.join("trace.db");
    let db = store::open(&path).await.unwrap();
    let sessions = dir.join("sessions");
    write(&db, &sessions, &event("session.create", 10, &[("name", "demo"), ("group", "g")])).await.unwrap();
    write(&db, &sessions, &event("exec", 11, &[("argv", "true")])).await.unwrap();
    write(&db, &sessions, &event("actor.exit", 20, &[])).await.unwrap();
    write(&db, &sessions, &event("session.stop", 30, &[])).await.unwrap();

    let loaded = load(&path, "s").await.unwrap();
    assert_eq!(loaded.session["name"], json!("demo"));
    assert_eq!(loaded.session["group"], json!("g"));
    // The actor exit ends the session; a later stop changes the state only.
    assert_eq!(loaded.session["ended_ms"], json!(20));
    assert_eq!(loaded.session["state"], json!("stopped"));
    assert_eq!(loaded.events.len(), 4);
    assert_eq!(loaded.events[1].1, "exec");
    assert_eq!(loaded.events[1].2["argv"], "true");
    let jsonl = std::fs::read_to_string(sessions.join("s1/trace.jsonl")).unwrap();
    assert_eq!(jsonl.lines().count(), 4);
  }

  #[tokio::test]
  async fn adopts_database_written_before_migrations() {
    // ROOT CAUSE:
    //
    // If a trace.db was created by the hand-written schema, migrating it would
    // fail because the tables already exist.
    //
    // Before SeaORM, the daemon created tables with plain DDL.
    // The fix keeps the old rows readable and appends to them.
    let dir = scratch("legacy");
    let path = dir.join("trace.db");
    let legacy = Database::connect(format!("sqlite://{}?mode=rwc", path.display())).await.unwrap();
    legacy
      .execute_unprepared(
        "CREATE TABLE events (id INTEGER PRIMARY KEY AUTOINCREMENT, session TEXT NOT NULL, time_ms INTEGER NOT NULL,
           kind TEXT NOT NULL, attrs TEXT NOT NULL);
         CREATE INDEX events_session ON events(session, id);
         CREATE TABLE sessions (id TEXT PRIMARY KEY, name TEXT, grp TEXT, profile TEXT, created_ms INTEGER, ended_ms INTEGER, state TEXT);
         INSERT INTO sessions VALUES ('s1', 'old', NULL, NULL, 5, NULL, 'running');
         INSERT INTO events (session, time_ms, kind, attrs) VALUES ('s1', 5, 'session.create', '{}');",
      )
      .await
      .unwrap();
    legacy.close().await.unwrap();

    let db = store::open(&path).await.unwrap();
    write(&db, &dir.join("sessions"), &event("session.stop", 9, &[])).await.unwrap();
    let loaded = load(&path, "latest").await.unwrap();
    assert_eq!(loaded.session["name"], json!("old"));
    assert_eq!(loaded.session["state"], json!("stopped"));
    assert_eq!(loaded.events.len(), 2);
  }
}
