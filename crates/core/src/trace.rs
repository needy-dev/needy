//! Session traces: SQLite for queries, JSONL per session for export.
//!
//! Writes go through a channel so recording never blocks the operation being
//! recorded. Producers record typed [`Event`]s; rows hold the kind, the
//! schema version and the JSON payload, and readers decode them with
//! [`Event::decode`].
//
// TODO(trace): this writer, `store` and the reader move to the `trace` crate
// in slice 4 of the module layout redesign, which carries an envelope with a
// JSON payload and does not depend on `core`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use sea_orm::sea_query::{Expr, Func, OnConflict};
use sea_orm::{ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder};
use serde::Serialize;
use serde_json::json;
use serde_json::value::RawValue;
use tokio::sync::mpsc;

use crate::event::{Event, SCHEMA_VERSION};
use crate::store::{self, event, session};
use crate::types::SessionState;

/// An event on its way to the writer.
#[derive(Debug, Clone)]
struct Entry {
  session: String,
  time_ms: i64,
  event: Event,
}

#[derive(Clone)]
pub struct Trace {
  tx: mpsc::UnboundedSender<Entry>,
}

pub fn now_ms() -> i64 {
  SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

impl Trace {
  pub async fn open(db: &Path, sessions_dir: PathBuf) -> Result<Self> {
    let db = store::open(db).await?;
    let (tx, mut rx) = mpsc::unbounded_channel::<Entry>();
    tokio::spawn(async move {
      while let Some(entry) = rx.recv().await {
        if let Err(err) = write(&db, &sessions_dir, &entry).await {
          tracing::warn!("trace {} for session {}: {err:#}", entry.event.kind(), entry.session);
        }
      }
    });
    Ok(Self { tx })
  }

  /// Records `event` for `session` now.
  pub fn record(&self, session: &str, event: Event) {
    self.record_at(session, now_ms(), event);
  }

  /// Records `event` with the time its producer gave it.
  pub fn record_at(&self, session: &str, time_ms: i64, event: Event) {
    let entry = Entry {
      session: session.to_string(),
      time_ms,
      event,
    };
    // The writer only stops when the runtime shuts down, and then nothing is
    // left to record.
    let _ = self.tx.send(entry);
  }
}

/// One line of `trace.jsonl`.
#[derive(Serialize)]
struct JsonlLine<'a> {
  time_ms: i64,
  kind: &'a str,
  version: u32,
  payload: &'a RawValue,
}

/// Stores one event, updates its session row and appends it to the session's
/// `trace.jsonl`.
async fn write(db: &DatabaseConnection, sessions_dir: &Path, entry: &Entry) -> Result<()> {
  let Entry {
    session,
    time_ms,
    event,
  } = entry;
  let kind = event.kind().as_str();
  let payload = event.payload();
  event::Entity::insert(event::ActiveModel {
    session: Set(session.clone()),
    time_ms: Set(*time_ms),
    kind: Set(kind.to_string()),
    version: Set(SCHEMA_VERSION as i32),
    payload: Set(payload.clone()),
    ..Default::default()
  })
  .exec(db)
  .await
  .context("insert event")?;
  match event {
    Event::SessionCreate(create) => {
      let row = session::ActiveModel {
        id: Set(session.clone()),
        name: Set(Some(create.name.clone())),
        grp: Set(Some(create.group.clone())),
        profile: Set(Some(create.profile.clone())),
        created_ms: Set(Some(*time_ms)),
        ended_ms: Set(None),
        state: Set(Some(SessionState::Running.as_str().into())),
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
    Event::SessionStop(_) | Event::ActorExit(_) => {
      let state = if let Event::SessionStop(_) = event {
        SessionState::Stopped
      } else {
        SessionState::Finished
      };
      // The first end wins: a stop after the actor exited keeps the exit time.
      let ended = Func::coalesce([Expr::col(session::Column::EndedMs), Expr::val(*time_ms)]);
      session::Entity::update_many()
        .col_expr(session::Column::EndedMs, ended.into())
        .col_expr(session::Column::State, Expr::val(state.as_str()))
        .filter(session::Column::Id.eq(session))
        .exec(db)
        .await
        .context("update session")?;
    }
    _ => {}
  }
  // Only sessions with a directory (those started by the daemon) export JSONL.
  let dir = sessions_dir.join(session);
  if !session.is_empty() && dir.is_dir() {
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("trace.jsonl")).context("open trace.jsonl")?;
    let line = JsonlLine {
      time_ms: *time_ms,
      kind,
      version: SCHEMA_VERSION,
      payload: &RawValue::from_string(payload).context("payload is not JSON")?,
    };
    writeln!(file, "{}", serde_json::to_string(&line)?).context("append trace.jsonl")?;
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

/// One stored event, not yet decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
  pub time_ms: i64,
  pub kind: String,
  /// The payload schema version; 0 for rows written before the typed schema.
  pub version: u32,
  pub payload: String,
}

pub struct Loaded {
  /// The full session ID.
  pub session: String,
  pub rows: Vec<Row>,
}

/// Reads one session's rows, or the latest session's, in recorded order.
/// `session` may be an id prefix.
pub async fn load(db: &Path, session: &str) -> Result<Loaded> {
  anyhow::ensure!(db.exists(), "no trace database at {}; start a daemon first", db.display());
  let db = store::open(db).await?;
  let mut query = session::Entity::find().order_by_desc(session::Column::CreatedMs);
  if session != "latest" {
    query = query.filter(session::Column::Id.starts_with(session));
  }
  let row = query.one(&db).await?.with_context(|| format!("no session matches {session}"))?;
  let rows = event::Entity::find()
    .filter(event::Column::Session.eq(&row.id))
    .order_by_asc(event::Column::Id)
    .all(&db)
    .await?
    .into_iter()
    .map(|e| Row {
      time_ms: e.time_ms,
      kind: e.kind,
      version: u32::try_from(e.version).unwrap_or(0),
      payload: e.payload,
    })
    .collect();
  Ok(Loaded {
    session: row.id,
    rows,
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::event::{ActorExit, ExecStart, SessionCreate, SessionStop};
  use crate::types::Origin;
  use sea_orm::{ConnectionTrait, Database};

  fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("needy-trace-{}-{name}", std::process::id()));
    // A directory left by an earlier run may or may not exist.
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("sessions/s1")).unwrap();
    dir
  }

  fn entry(time_ms: i64, event: Event) -> Entry {
    Entry {
      session: "s1".into(),
      time_ms,
      event,
    }
  }

  fn exec_start() -> Event {
    Event::ExecStart(ExecStart {
      process: "p1".into(),
      executor: "local".into(),
      argv: vec!["true".into()],
      origin: Origin::Cli,
      cast: None,
    })
  }

  #[tokio::test]
  async fn records_session_lifecycle_and_loads_by_prefix() {
    let dir = scratch("lifecycle");
    let path = dir.join("trace.db");
    let db = store::open(&path).await.unwrap();
    let sessions = dir.join("sessions");
    let create = Event::SessionCreate(SessionCreate {
      name: "demo".into(),
      group: "g".into(),
      profile: "smoke".into(),
      executors: vec![],
    });
    write(&db, &sessions, &entry(10, create)).await.unwrap();
    write(&db, &sessions, &entry(11, exec_start())).await.unwrap();
    write(
      &db,
      &sessions,
      &entry(
        20,
        Event::ActorExit(ActorExit {
          code: Some(0),
          signal: None,
        }),
      ),
    )
    .await
    .unwrap();
    write(&db, &sessions, &entry(30, Event::SessionStop(SessionStop {}))).await.unwrap();

    let row = session::Entity::find_by_id("s1").one(&db).await.unwrap().unwrap();
    assert_eq!(row.name.as_deref(), Some("demo"));
    assert_eq!(row.grp.as_deref(), Some("g"));
    assert_eq!(row.profile.as_deref(), Some("smoke"));
    // The actor exit ends the session; a later stop changes the state only.
    assert_eq!(row.ended_ms, Some(20));
    assert_eq!(row.state.as_deref(), Some("stopped"));

    let loaded = load(&path, "s").await.unwrap();
    assert_eq!(loaded.session, "s1");
    assert_eq!(loaded.rows.len(), 4);
    let row = &loaded.rows[1];
    assert_eq!((row.time_ms, row.kind.as_str(), row.version), (11, "exec.start", SCHEMA_VERSION));
    assert_eq!(Event::decode(&row.kind, row.version, &row.payload).unwrap(), exec_start());
    let jsonl = std::fs::read_to_string(sessions.join("s1/trace.jsonl")).unwrap();
    assert_eq!(jsonl.lines().count(), 4);
    let line: serde_json::Value = serde_json::from_str(jsonl.lines().nth(1).unwrap()).unwrap();
    assert_eq!(line["kind"], "exec.start");
    assert_eq!(line["payload"]["origin"], "cli");
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
         INSERT INTO events (session, time_ms, kind, attrs) VALUES ('s1', 5, 'session.create', '{\"name\":\"old\"}');",
      )
      .await
      .unwrap();
    legacy.close().await.unwrap();

    let db = store::open(&path).await.unwrap();
    write(&db, &dir.join("sessions"), &entry(9, Event::SessionStop(SessionStop {}))).await.unwrap();
    let row = session::Entity::find_by_id("s1").one(&db).await.unwrap().unwrap();
    assert_eq!(row.name.as_deref(), Some("old"));
    assert_eq!(row.state.as_deref(), Some("stopped"));
    let loaded = load(&path, "latest").await.unwrap();
    assert_eq!(loaded.rows.len(), 2);
    // Rows from before the typed schema keep their attributes as version 0,
    // which readers reject instead of misreading.
    assert_eq!((loaded.rows[0].version, loaded.rows[0].payload.as_str()), (0, r#"{"name":"old"}"#));
    assert!(Event::decode(&loaded.rows[0].kind, loaded.rows[0].version, &loaded.rows[0].payload).is_err());
    assert_eq!(loaded.rows[1].version, SCHEMA_VERSION);
  }
}
