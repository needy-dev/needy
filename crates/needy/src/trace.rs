//! Session traces: SQLite for queries, JSONL per session for export.
//!
//! Writes go through a channel so recording never blocks the operation being
//! recorded.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use serde_json::{Value, json};
use tokio::sync::mpsc;

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
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

impl Trace {
    pub fn open(db: &Path, sessions_dir: PathBuf) -> Result<Self> {
        let conn = rusqlite::Connection::open(db)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS events (
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               session TEXT NOT NULL,
               time_ms INTEGER NOT NULL,
               kind TEXT NOT NULL,
               attrs TEXT NOT NULL);
             CREATE INDEX IF NOT EXISTS events_session ON events(session, id);
             CREATE TABLE IF NOT EXISTS sessions (
               id TEXT PRIMARY KEY, name TEXT, grp TEXT, profile TEXT,
               created_ms INTEGER, ended_ms INTEGER, state TEXT);",
        )?;
        let (tx, mut rx) = mpsc::unbounded_channel::<Event>();
        std::thread::spawn(move || {
            while let Some(event) = rx.blocking_recv() {
                let attrs = serde_json::to_string(&event.attrs).unwrap_or_default();
                let _ = conn.execute(
                    "INSERT INTO events (session, time_ms, kind, attrs) VALUES (?1, ?2, ?3, ?4)",
                    rusqlite::params![event.session, event.time_ms, event.kind, attrs],
                );
                match event.kind.as_str() {
                    "session.create" => {
                        let _ = conn.execute(
                            "INSERT OR REPLACE INTO sessions (id, name, grp, profile, created_ms, state) VALUES (?1, ?2, ?3, ?4, ?5, 'running')",
                            rusqlite::params![
                                event.session,
                                event.attrs.get("name"),
                                event.attrs.get("group"),
                                event.attrs.get("profile"),
                                event.time_ms
                            ],
                        );
                    }
                    "session.stop" | "actor.exit" => {
                        let _ = conn.execute(
                            "UPDATE sessions SET ended_ms = COALESCE(ended_ms, ?2), state = ?3 WHERE id = ?1",
                            rusqlite::params![event.session, event.time_ms, if event.kind == "session.stop" { "stopped" } else { "finished" }],
                        );
                    }
                    _ => {}
                }
                if !event.session.is_empty() {
                    let dir = sessions_dir.join(&event.session);
                    if dir.is_dir()
                        && let Ok(mut file) = std::fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(dir.join("trace.jsonl"))
                    {
                        let line = json!({"time_ms": event.time_ms, "kind": event.kind, "attrs": event.attrs});
                        let _ = writeln!(file, "{line}");
                    }
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
        let _ = self.tx.send(Event {
            session: session.to_string(),
            time_ms: now_ms(),
            kind: kind.to_string(),
            attrs: attrs
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        });
    }

    pub fn record_event(&self, event: Event) {
        let _ = self.tx.send(event);
    }
}

pub struct Loaded {
    pub session: BTreeMap<String, Value>,
    pub events: Vec<(i64, String, BTreeMap<String, String>)>,
}

/// Reads one session, or the latest one, for reports.
pub fn load(db: &Path, session: &str) -> Result<Loaded> {
    let conn = rusqlite::Connection::open(db)?;
    let id: String = if session == "latest" {
        conn.query_row(
            "SELECT id FROM sessions ORDER BY created_ms DESC LIMIT 1",
            [],
            |r| r.get(0),
        )?
    } else {
        conn.query_row(
            "SELECT id FROM sessions WHERE id LIKE ?1 || '%' ORDER BY created_ms DESC LIMIT 1",
            [session],
            |r| r.get(0),
        )?
    };
    let mut info = BTreeMap::new();
    conn.query_row(
        "SELECT id, name, grp, profile, created_ms, ended_ms, state FROM sessions WHERE id = ?1",
        [&id],
        |r| {
            info.insert("id".into(), json!(r.get::<_, String>(0)?));
            info.insert("name".into(), json!(r.get::<_, Option<String>>(1)?));
            info.insert("group".into(), json!(r.get::<_, Option<String>>(2)?));
            info.insert("profile".into(), json!(r.get::<_, Option<String>>(3)?));
            info.insert("created_ms".into(), json!(r.get::<_, Option<i64>>(4)?));
            info.insert("ended_ms".into(), json!(r.get::<_, Option<i64>>(5)?));
            info.insert("state".into(), json!(r.get::<_, Option<String>>(6)?));
            Ok(())
        },
    )?;
    let mut statement =
        conn.prepare("SELECT time_ms, kind, attrs FROM events WHERE session = ?1 ORDER BY id")?;
    let events = statement
        .query_map([&id], |r| {
            let attrs: String = r.get(2)?;
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                serde_json::from_str(&attrs).unwrap_or_default(),
            ))
        })?
        .filter_map(|r| r.ok())
        .collect();
    Ok(Loaded {
        session: info,
        events,
    })
}
