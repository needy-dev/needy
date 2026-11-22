//! Integration tests for `features/opencode/needy-merge-sqlite`, tested
//! directly as that feature's own code with real SQLite3 databases and
//! write-ahead logs (`-wal`) present.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

fn driver_script() -> PathBuf {
  Path::new(env!("CARGO_MANIFEST_DIR"))
    .join("../../features/opencode/needy-merge-sqlite")
    .canonicalize()
    .expect("features/opencode/needy-merge-sqlite must exist")
}

/// Runs an inline Python helper that uses the standard library `sqlite3`
/// module so tests work identically on macOS and Linux without extra crates.
fn run_py(script: &str, args: &[&Path]) -> std::process::Output {
  let mut cmd = Command::new("python3");
  cmd.arg("-c").arg(script);
  for arg in args {
    cmd.arg(arg);
  }
  cmd.output().expect("python3 must be available to run opencode SQLite merge tests")
}

fn run_driver(seed: &Path, run: &Path, store: &Path, out: &Path) -> std::process::Output {
  Command::new("sh").arg(driver_script()).arg(seed).arg(run).arg(store).arg(out).output().expect("failed to execute needy-merge-sqlite")
}

#[test]
fn concurrent_runs_inserting_different_sessions_with_wal_merge_cleanly() {
  let dir = tempfile::tempdir().unwrap();
  let [seed, run, store, out] = ["seed", "run", "store", "out"].map(|n| dir.path().join(n));
  for d in [&seed, &run, &store, &out] {
    std::fs::create_dir_all(d).unwrap();
  }

  // Create seed DB in WAL mode with one existing session and message, then
  // copy into `run` and `store` and insert distinct sessions into each with
  // `wal_autocheckpoint = 0` so the new rows live in `-wal` files!
  let setup = r#"
import shutil, sqlite3, sys
from pathlib import Path

seed_db = Path(sys.argv[1]) / "opencode.db"
run_db = Path(sys.argv[2]) / "opencode.db"
store_db = Path(sys.argv[3]) / "opencode.db"

conn = sqlite3.connect(str(seed_db))
conn.execute("PRAGMA journal_mode=WAL")
conn.execute("CREATE TABLE session (id TEXT PRIMARY KEY, title TEXT NOT NULL)")
conn.execute("CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT NOT NULL, body TEXT NOT NULL)")
conn.execute("INSERT INTO session VALUES ('s0', 'seed session')")
conn.execute("INSERT INTO message VALUES ('m0', 's0', 'hello seed')")
conn.commit()
conn.execute("PRAGMA wal_checkpoint(TRUNCATE)")
conn.close()

shutil.copy2(seed_db, run_db)
shutil.copy2(seed_db, store_db)

# Keep a second connection open while inserting so SQLite leaves the -wal file
# uncheckpointed on disk, just like an active or abruptly stopped harness.
r1 = sqlite3.connect(str(run_db))
r1.execute("PRAGMA journal_mode=WAL")
r1.execute("PRAGMA wal_autocheckpoint=0")
r_hold = sqlite3.connect(str(run_db))
r_hold.execute("BEGIN")
r_hold.execute("SELECT count(*) FROM session").fetchall()
r1.execute("INSERT INTO session VALUES ('s1', 'run 1 session')")
r1.execute("INSERT INTO message VALUES ('m1', 's1', 'from run 1')")
r1.commit()
# Copy the live db + -wal + -shm snapshot before closing r_hold.
r1.close()
r_snap = Path(sys.argv[2]) / "snap"
r_snap.mkdir()
for p in Path(sys.argv[2]).glob("opencode.db*"):
  shutil.copy2(p, r_snap / p.name)
r_hold.close()
for p in Path(sys.argv[2]).glob("opencode.db*"):
  p.unlink()
for p in r_snap.glob("opencode.db*"):
  shutil.move(str(p), str(Path(sys.argv[2]) / p.name))
r_snap.rmdir()

s1 = sqlite3.connect(str(store_db))
s1.execute("PRAGMA journal_mode=WAL")
s1.execute("PRAGMA wal_autocheckpoint=0")
s_hold = sqlite3.connect(str(store_db))
s_hold.execute("BEGIN")
s_hold.execute("SELECT count(*) FROM session").fetchall()
s1.execute("INSERT INTO session VALUES ('s2', 'run 2 session')")
s1.execute("INSERT INTO message VALUES ('m2', 's2', 'from run 2')")
s1.commit()
s1.close()
s_snap = Path(sys.argv[3]) / "snap"
s_snap.mkdir()
for p in Path(sys.argv[3]).glob("opencode.db*"):
  shutil.copy2(p, s_snap / p.name)
s_hold.close()
for p in Path(sys.argv[3]).glob("opencode.db*"):
  p.unlink()
for p in s_snap.glob("opencode.db*"):
  shutil.move(str(p), str(Path(sys.argv[3]) / p.name))
s_snap.rmdir()
"#;
  let out_setup = run_py(setup, &[&seed, &run, &store]);
  assert!(out_setup.status.success(), "setup failed: {}", String::from_utf8_lossy(&out_setup.stderr));

  // Verify WAL side files are actually present and non-empty before merging!
  let run_wal = run.join("opencode.db-wal");
  let store_wal = store.join("opencode.db-wal");
  assert!(run_wal.is_file() && std::fs::metadata(&run_wal).unwrap().len() > 0, "run -wal must be present");
  assert!(store_wal.is_file() && std::fs::metadata(&store_wal).unwrap().len() > 0, "store -wal must be present");

  let result = run_driver(&seed, &run, &store, &out);
  assert!(result.status.success(), "needy-merge-sqlite failed: {}", String::from_utf8_lossy(&result.stderr));
  assert!(out.join("opencode.db").is_file(), "merged opencode.db must exist");
  assert!(!out.join("opencode.db-wal").exists(), "merged output must be checkpointed with no -wal");

  // Verify integrity_check and all three sessions + messages are present.
  let verify = r#"
import sqlite3, sys
conn = sqlite3.connect(sys.argv[1])
assert conn.execute("PRAGMA integrity_check").fetchall() == [("ok",)]
sessions = sorted(conn.execute("SELECT id, title FROM session").fetchall())
messages = sorted(conn.execute("SELECT id, session_id, body FROM message").fetchall())
assert sessions == [
  ("s0", "seed session"),
  ("s1", "run 1 session"),
  ("s2", "run 2 session"),
], f"unexpected sessions: {sessions}"
assert messages == [
  ("m0", "s0", "hello seed"),
  ("m1", "s1", "from run 1"),
  ("m2", "s2", "from run 2"),
], f"unexpected messages: {messages}"
"#;
  let out_verify = run_py(verify, &[&out.join("opencode.db")]);
  assert!(out_verify.status.success(), "verify failed: {}", String::from_utf8_lossy(&out_verify.stderr));
}

#[test]
fn autoincrement_table_is_reported_rather_than_merged() {
  let dir = tempfile::tempdir().unwrap();
  let [seed, run, store, out] = ["seed", "run", "store", "out"].map(|n| dir.path().join(n));
  for d in [&seed, &run, &store, &out] {
    std::fs::create_dir_all(d).unwrap();
  }

  let setup = r#"
import shutil, sqlite3, sys
from pathlib import Path

seed_db = Path(sys.argv[1]) / "opencode.db"
run_db = Path(sys.argv[2]) / "opencode.db"
store_db = Path(sys.argv[3]) / "opencode.db"

conn = sqlite3.connect(str(seed_db))
conn.execute("CREATE TABLE events (id INTEGER PRIMARY KEY AUTOINCREMENT, body TEXT NOT NULL)")
conn.commit()
conn.close()

shutil.copy2(seed_db, run_db)
shutil.copy2(seed_db, store_db)

for db, body in [(run_db, "from run"), (store_db, "from store")]:
  c = sqlite3.connect(str(db))
  c.execute("INSERT INTO events (body) VALUES (?)", (body,))
  c.commit()
  c.close()
"#;
  let out_setup = run_py(setup, &[&seed, &run, &store]);
  assert!(out_setup.status.success(), "setup failed: {}", String::from_utf8_lossy(&out_setup.stderr));

  let result = run_driver(&seed, &run, &store, &out);
  let stderr = String::from_utf8_lossy(&result.stderr);
  assert!(!result.status.success(), "AUTOINCREMENT table must fail rather than silently colliding IDs");
  assert!(stderr.contains("AUTOINCREMENT"), "stderr must mention AUTOINCREMENT: {stderr}");
  assert!(stderr.contains("events"), "stderr must name the table: {stderr}");
  assert!(!out.join("opencode.db").exists(), "failed merge must not write output DB");
}

#[test]
fn conflicting_row_update_on_same_primary_key_is_reported() {
  let dir = tempfile::tempdir().unwrap();
  let [seed, run, store, out] = ["seed", "run", "store", "out"].map(|n| dir.path().join(n));
  for d in [&seed, &run, &store, &out] {
    std::fs::create_dir_all(d).unwrap();
  }

  let setup = r#"
import shutil, sqlite3, sys
from pathlib import Path

seed_db = Path(sys.argv[1]) / "opencode.db"
run_db = Path(sys.argv[2]) / "opencode.db"
store_db = Path(sys.argv[3]) / "opencode.db"

conn = sqlite3.connect(str(seed_db))
conn.execute("CREATE TABLE session (id TEXT PRIMARY KEY, title TEXT NOT NULL)")
conn.execute("INSERT INTO session VALUES ('s0', 'seed')")
conn.commit()
conn.close()

shutil.copy2(seed_db, run_db)
shutil.copy2(seed_db, store_db)

for db, title in [(run_db, "changed by run"), (store_db, "changed by store")]:
  c = sqlite3.connect(str(db))
  c.execute("UPDATE session SET title = ? WHERE id = 's0'", (title,))
  c.commit()
  c.close()
"#;
  let out_setup = run_py(setup, &[&seed, &run, &store]);
  assert!(out_setup.status.success(), "setup failed: {}", String::from_utf8_lossy(&out_setup.stderr));

  let result = run_driver(&seed, &run, &store, &out);
  let stderr = String::from_utf8_lossy(&result.stderr);
  assert!(!result.status.success(), "conflicting PK update must exit non-zero");
  assert!(stderr.contains("session"), "stderr must name the table: {stderr}");
  assert!(stderr.contains("s0"), "stderr must name the conflicting PK: {stderr}");
  assert!(!out.join("opencode.db").exists(), "failed merge must not write output DB");
}

#[test]
fn single_side_creation_and_composite_pk_with_special_values_merge_cleanly() {
  let dir = tempfile::tempdir().unwrap();
  let [seed, run, store, out] = ["seed", "run", "store", "out"].map(|n| dir.path().join(n));
  for d in [&seed, &run, &store, &out] {
    std::fs::create_dir_all(d).unwrap();
  }

  // 1. First run creates opencode.db when seed and store are both empty.
  let setup_first = r#"
import sqlite3, sys
from pathlib import Path

run_db = Path(sys.argv[1]) / "opencode.db"
conn = sqlite3.connect(str(run_db))
conn.execute("CREATE TABLE kv (ns TEXT NOT NULL, k TEXT NOT NULL, v TEXT, raw BLOB, PRIMARY KEY (ns, k))")
conn.execute("INSERT INTO kv VALUES ('auth', 'tok', 'line1\n''quoted'' \"val\"', ?)", (b"\x00\x01\xff",))
conn.execute("INSERT INTO kv VALUES ('auth', 'opt', NULL, NULL)")
conn.commit()
conn.close()
"#;
  let res = run_py(setup_first, &[&run]);
  assert!(res.status.success(), "setup failed: {}", String::from_utf8_lossy(&res.stderr));

  let first = run_driver(&seed, &run, &store, &out);
  assert!(first.status.success(), "first merge failed: {}", String::from_utf8_lossy(&first.stderr));
  assert!(out.join("opencode.db").is_file());

  // 2. Concurrent runs on that DB: one deletes ('auth','opt') and inserts ('auth','r1'),
  //    store inserts ('auth','s1') with quotes/newlines/BLOB.
  std::fs::copy(out.join("opencode.db"), seed.join("opencode.db")).unwrap();
  std::fs::copy(out.join("opencode.db"), store.join("opencode.db")).unwrap();
  std::fs::remove_file(out.join("opencode.db")).unwrap();

  let setup_second = r#"
import sqlite3, sys
from pathlib import Path

run_db = Path(sys.argv[1]) / "opencode.db"
store_db = Path(sys.argv[2]) / "opencode.db"

r = sqlite3.connect(str(run_db))
r.execute("DELETE FROM kv WHERE ns = 'auth' AND k = 'opt'")
r.execute("INSERT INTO kv VALUES ('auth', 'r1', 'hello\nworld', ?)", (b"\xde\xad",))
r.commit()
r.close()

s = sqlite3.connect(str(store_db))
s.execute("INSERT INTO kv VALUES ('auth', 's1', 'store''s value', ?)", (b"\xbe\xef",))
s.commit()
s.close()
"#;
  let res2 = run_py(setup_second, &[&run, &store]);
  assert!(res2.status.success(), "setup2 failed: {}", String::from_utf8_lossy(&res2.stderr));

  let second = run_driver(&seed, &run, &store, &out);
  assert!(second.status.success(), "second merge failed: {}", String::from_utf8_lossy(&second.stderr));

  let verify = r#"
import sqlite3, sys
conn = sqlite3.connect(sys.argv[1])
assert conn.execute("PRAGMA integrity_check").fetchall() == [("ok",)]
rows = sorted(conn.execute("SELECT ns, k, v, raw FROM kv").fetchall())
assert rows == [
  ("auth", "r1", "hello\nworld", b"\xde\xad"),
  ("auth", "s1", "store's value", b"\xbe\xef"),
  ("auth", "tok", "line1\n'quoted' \"val\"", b"\x00\x01\xff"),
], f"unexpected rows: {rows!r}"
"#;
  let v = run_py(verify, &[&out.join("opencode.db")]);
  assert!(v.status.success(), "verify failed: {}", String::from_utf8_lossy(&v.stderr));
}
