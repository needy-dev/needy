use serde_json::value::to_raw_value;

use super::*;

fn event(seq: u64, kind: &str, payload: serde_json::Value) -> Event {
  Event {
    run: "s1".into(),
    seq,
    time_ms: 1_000 + seq as i64,
    kind: kind.into(),
    version: 1,
    payload: to_raw_value(&payload).unwrap(),
  }
}

#[tokio::test]
async fn writes_one_envelope_per_line_and_appends() {
  let dir = tempfile::tempdir().unwrap();
  let path = dir.path().join("runs/s1/trace.jsonl");

  let mut sink = Jsonl::create(&path).unwrap();
  sink.write(&event(0, "run.create", serde_json::json!({"name": "demo"}))).await.unwrap();
  drop(sink);
  let mut sink = Jsonl::create(&path).unwrap();
  sink.write(&event(1, "trace.gap", serde_json::json!({"dropped": 2}))).await.unwrap();

  let text = std::fs::read_to_string(&path).unwrap();
  let lines: Vec<&str> = text.lines().collect();
  assert_eq!(
    lines,
    [
      r#"{"run":"s1","seq":0,"time_ms":1000,"kind":"run.create","version":1,"payload":{"name":"demo"}}"#,
      r#"{"run":"s1","seq":1,"time_ms":1001,"kind":"trace.gap","version":1,"payload":{"dropped":2}}"#,
    ]
  );
}
