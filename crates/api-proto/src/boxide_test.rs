use std::path::Path;

use prost::Message;

use crate::boxide::v1::{ExecStart, Frame, SessionRequest, StreamOpen, frame, stream_open};

// Boxide is kept free of needy's schema so it can move to its own repository
// unchanged (spec §10). A schema that imports or names `needy.v1` breaks that.
#[test]
fn boxide_schema_names_nothing_of_needy() {
  let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../proto/boxide/v1");
  let mut checked = 0;
  for entry in std::fs::read_dir(&dir).expect("proto/boxide/v1 exists") {
    let path = entry.expect("readable entry").path();
    if path.extension().is_none_or(|ext| ext != "proto") {
      continue;
    }
    let text = std::fs::read_to_string(&path).expect("readable schema");
    for (number, line) in text.lines().enumerate() {
      let code = line.split("//").next().unwrap_or_default();
      assert!(!code.contains("needy"), "{}:{} refers to needy: {line}", path.display(), number + 1);
    }
    checked += 1;
  }
  assert_eq!(checked, 3, "session.proto, exec.proto and fs.proto");
}

// The gRPC binding wraps each frame in `SessionRequest`; the WebSocket and
// stdio bindings carry the same `Frame` bytes directly (spec §3).
#[test]
fn grpc_binding_carries_the_same_frame() {
  let frame = Frame {
    stream: 1,
    body: Some(frame::Body::Open(StreamOpen {
      kind: Some(stream_open::Kind::Exec(ExecStart {
        argv: vec!["git".into(), "status".into()],
        cwd: "/work".into(),
        ..Default::default()
      })),
    })),
    extensions: [("dev.needy.run".to_string(), b"r-1".to_vec())].into(),
  };

  let wrapped = SessionRequest {
    frame: Some(frame.clone()),
  }
  .encode_to_vec();
  let unwrapped = SessionRequest::decode(wrapped.as_slice()).expect("decodes").frame;

  assert_eq!(unwrapped.as_ref(), Some(&frame));
  assert_eq!(Frame::decode(frame.encode_to_vec().as_slice()).expect("decodes"), frame);
}
