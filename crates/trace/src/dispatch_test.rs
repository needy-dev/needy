use serde_json::value::{RawValue, to_raw_value};

use super::*;
use crate::sink::memory::Memory;

fn payload(n: u64) -> Box<RawValue> {
  to_raw_value(&serde_json::json!({ "n": n })).unwrap()
}

fn dispatch(memory: &Memory, capacity: usize) -> Dispatch {
  Dispatch::new(
    "s1",
    vec![Box::new(memory.clone())],
    Options {
      capacity,
      version: 3,
      timing: false,
    },
  )
}

fn kinds(events: &[Event]) -> Vec<&str> {
  events.iter().map(|e| e.kind.as_str()).collect()
}

#[tokio::test]
async fn events_keep_their_order_and_number_from_zero() {
  let memory = Memory::new();
  let trace = dispatch(&memory, 16);

  trace.emit("a.one", payload(1));
  trace.emit_at(42, "a.two", payload(2));
  trace.flush().await;

  let events = memory.events();
  assert_eq!(kinds(&events), ["a.one", "a.two"]);
  assert_eq!(events.iter().map(|e| e.seq).collect::<Vec<_>>(), [0, 1]);
  assert!(events.iter().all(|e| e.run == "s1" && e.version == 3));
  assert_eq!(events[1].time_ms, 42);
  assert_eq!(events[1].payload.get(), r#"{"n":2}"#);
}

// The test runtime has one thread, so the worker cannot drain the queue
// until the test awaits: emitting past the capacity must drop instead of
// blocking, or the test would hang.
#[tokio::test]
async fn a_full_queue_drops_and_writes_one_gap_when_space_returns() {
  let memory = Memory::new();
  let trace = dispatch(&memory, 4);

  for n in 0..10 {
    trace.emit("a.event", payload(n));
  }
  trace.flush().await;
  trace.emit("a.after", payload(10));
  trace.flush().await;

  let events = memory.events();
  assert_eq!(
    kinds(&events),
    [
      "a.event",
      "a.event",
      "a.event",
      "a.event",
      event::GAP,
      "a.after"
    ]
  );
  let gaps: Vec<_> = events.iter().filter(|e| e.kind == event::GAP).collect();
  assert_eq!(gaps.len(), 1);
  assert_eq!(gaps[0].payload.get(), r#"{"dropped":6}"#);
  assert_eq!(gaps[0].version, 3);
  assert_eq!(events.iter().map(|e| e.seq).collect::<Vec<_>>(), [0, 1, 2, 3, 4, 5]);
  // The events that made it are the first ones, unchanged.
  assert_eq!(events[3].payload.get(), r#"{"n":3}"#);
}

#[tokio::test]
async fn a_gap_is_written_before_the_next_event_when_the_queue_has_room_again() {
  let memory = Memory::new();
  let trace = dispatch(&memory, 2);

  for n in 0..5 {
    trace.emit("a.event", payload(n));
  }
  // Let the worker drain without a flush; the next emit finds room.
  while memory.events().len() < 2 {
    tokio::task::yield_now().await;
  }
  tokio::task::yield_now().await;
  trace.emit("a.after", payload(5));
  trace.flush().await;

  let events = memory.events();
  assert_eq!(kinds(&events), ["a.event", "a.event", event::GAP, "a.after"]);
  assert_eq!(events[2].payload.get(), r#"{"dropped":3}"#);
}

#[tokio::test]
async fn dropping_the_last_handle_writes_a_pending_gap() {
  let memory = Memory::new();
  let trace = dispatch(&memory, 1);

  for n in 0..3 {
    trace.emit("a.event", payload(n));
  }
  drop(trace);
  // The worker drains and ends on its own.
  for _ in 0..100 {
    if memory.events().len() == 2 {
      break;
    }
    tokio::task::yield_now().await;
  }

  let events = memory.events();
  assert_eq!(kinds(&events), ["a.event", event::GAP]);
  assert_eq!(events[1].payload.get(), r#"{"dropped":2}"#);
}

#[tokio::test]
async fn timing_writes_per_hook_latencies_on_flush() {
  let memory = Memory::new();
  let trace = Dispatch::new(
    "s1",
    vec![Box::new(memory.clone())],
    Options {
      capacity: 16,
      version: 1,
      timing: true,
    },
  );

  trace.time("exec.decide", std::time::Duration::from_micros(30));
  trace.time("exec.decide", std::time::Duration::from_micros(10));
  trace.time("net.request", std::time::Duration::from_millis(2));
  trace.flush().await;
  trace.flush().await;

  let events = memory.events();
  assert_eq!(kinds(&events), [event::TIMING], "one summary; an empty one is not written");
  let hooks: serde_json::Value = serde_json::from_str(events[0].payload.get()).unwrap();
  assert_eq!(
    hooks,
    serde_json::json!({"hooks": {
      "exec.decide": {"count": 2, "total_us": 40, "min_us": 10, "max_us": 30},
      "net.request": {"count": 1, "total_us": 2000, "min_us": 2000, "max_us": 2000},
    }})
  );
}

#[tokio::test]
async fn timing_is_off_unless_the_options_turn_it_on() {
  let memory = Memory::new();
  let trace = dispatch(&memory, 16);

  trace.time("exec.decide", std::time::Duration::from_micros(30));
  trace.flush().await;

  assert!(memory.events().is_empty());
}
