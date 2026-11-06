use crate::model::{Event, Run, Session};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

#[derive(Clone, Serialize, Deserialize)]
pub struct Client {
    pub id: String,
    pub name: String,
    pub created_at: String,
    pub token_hash: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Record {
    pub run: Run,
    pub container: String,
    pub request_key: String,
    pub request_hash: String,
    #[serde(default)]
    pub stop_requested: bool,
    #[serde(skip)]
    pub events: Vec<Event>,
    #[serde(skip)]
    pub source_line: u64,
}

#[derive(Serialize, Deserialize)]
pub struct Disk {
    pub schema_version: u32,
    pub instance_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consumed_pairing_hash: Option<String>,
    #[serde(default)]
    pub consumed_pairings: BTreeMap<String, i64>,
    pub clients: BTreeMap<String, Client>,
    pub sessions: BTreeMap<String, Session>,
    pub runs: BTreeMap<String, Record>,
    #[serde(skip)]
    pub directory: PathBuf,
}

pub fn private_directory(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

pub fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let parent = path.parent().context("state path has no parent")?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary
        .as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    serde_json::to_writer(&mut temporary, value)?;
    temporary.write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|e| e.error)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

pub fn digest(value: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(value.as_ref()))
}
pub fn token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

impl Disk {
    pub fn open(directory: &Path) -> Result<Self> {
        private_directory(directory)?;
        let path = directory.join("rust-state.json");
        let mut disk = if path.exists() {
            let mut disk: Self =
                serde_json::from_reader(File::open(&path)?).context("invalid Rust state")?;
            ensure!(disk.schema_version == 1, "unsupported state schema");
            disk.directory = directory.into();
            disk
        } else {
            ensure!(
                !directory.join("state.json").exists(),
                "Go state directory requires explicit migration; use a new Rust data directory"
            );
            Self {
                schema_version: 1,
                instance_id: crate::model::id(),
                consumed_pairing_hash: None,
                consumed_pairings: BTreeMap::new(),
                clients: BTreeMap::new(),
                sessions: BTreeMap::new(),
                runs: BTreeMap::new(),
                directory: directory.into(),
            }
        };
        for (id, record) in &mut disk.runs {
            let journal = directory.join("runs").join(id).join("events.jsonl");
            if journal.exists() {
                let bytes = std::fs::read(&journal)?;
                let complete = bytes
                    .iter()
                    .rposition(|b| *b == b'\n')
                    .map(|n| n + 1)
                    .unwrap_or(0);
                let file = OpenOptions::new().write(true).open(&journal)?;
                if complete < bytes.len() {
                    file.set_len(complete as u64)?;
                    file.sync_all()?;
                }
                for line in bytes[..complete]
                    .split(|b| *b == b'\n')
                    .filter(|line| !line.is_empty())
                {
                    let event: Event =
                        serde_json::from_slice(line).context("corrupt event journal")?;
                    ensure!(
                        event.run_id == *id && event.id == record.events.len() as u64 + 1,
                        "invalid event sequence"
                    );
                    if let Some(source) = event.source_line {
                        record.source_line = source;
                    }
                    record.events.push(event);
                }
            }
        }
        disk.save()?;
        Ok(disk)
    }

    pub fn save(&self) -> Result<()> {
        atomic_json(&self.directory.join("rust-state.json"), self)
    }

    pub fn event(
        &mut self,
        id: &str,
        kind: &str,
        data: serde_json::Value,
        source_line: Option<u64>,
    ) -> Result<()> {
        let record = self.runs.get_mut(id).context("run not found")?;
        let event = Event {
            id: record.events.len() as u64 + 1,
            run_id: id.into(),
            kind: kind.into(),
            data,
            source_line,
        };
        let directory = self.directory.join("runs").join(id);
        private_directory(&directory)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(directory.join("events.jsonl"))?;
        let mut bytes = serde_json::to_vec(&event)?;
        bytes.push(b'\n');
        file.write_all(&bytes)?;
        file.sync_data()?;
        if let Some(source) = source_line {
            record.source_line = source;
        }
        record.events.push(event);
        self.save()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Run, now};

    fn run(disk: &mut Disk) {
        disk.runs.insert(
            "run".into(),
            Record {
                run: Run {
                    id: "run".into(),
                    session_id: "session".into(),
                    state: "running".into(),
                    created_at: now(),
                    exit_code: None,
                    error: None,
                },
                container: "owned-container".into(),
                request_key: "request-key".into(),
                request_hash: "request-hash".into(),
                stop_requested: false,
                events: Vec::new(),
                source_line: 0,
            },
        );
    }

    #[test]
    fn restart_recovers_complete_events_and_discards_only_incomplete_tail() {
        let directory = tempfile::tempdir().unwrap();
        let mut disk = Disk::open(directory.path()).unwrap();
        run(&mut disk);
        disk.event(
            "run",
            "output",
            serde_json::json!({"line":"first"}),
            Some(1),
        )
        .unwrap();
        let journal = directory.path().join("runs/run/events.jsonl");
        let mut file = OpenOptions::new().append(true).open(&journal).unwrap();
        file.write_all(b"{\"interrupted\":").unwrap();
        drop(file);
        drop(disk);
        let mut disk = Disk::open(directory.path()).unwrap();
        assert_eq!(disk.runs["run"].events.len(), 1);
        assert_eq!(disk.runs["run"].source_line, 1);
        disk.event(
            "run",
            "output",
            serde_json::json!({"line":"second"}),
            Some(2),
        )
        .unwrap();
        drop(disk);
        let disk = Disk::open(directory.path()).unwrap();
        assert_eq!(disk.runs["run"].events[1].id, 2);
        assert_eq!(disk.runs["run"].events[1].data["line"], "second");
        assert_eq!(disk.runs["run"].source_line, 2);
    }

    #[test]
    fn completed_journal_corruption_is_reported_instead_of_erased() {
        let directory = tempfile::tempdir().unwrap();
        let mut disk = Disk::open(directory.path()).unwrap();
        run(&mut disk);
        disk.event(
            "run",
            "output",
            serde_json::json!({"line":"first"}),
            Some(1),
        )
        .unwrap();
        drop(disk);
        let journal = directory.path().join("runs/run/events.jsonl");
        OpenOptions::new()
            .append(true)
            .open(&journal)
            .unwrap()
            .write_all(b"invalid complete entry\n")
            .unwrap();
        assert!(
            Disk::open(directory.path())
                .err()
                .unwrap()
                .to_string()
                .contains("corrupt event journal")
        );
        assert!(
            std::fs::read(journal)
                .unwrap()
                .ends_with(b"invalid complete entry\n")
        );
    }
}
