//! Image-pull output against a temporary Docker-compatible HTTP endpoint.

use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::sync::oneshot;

struct Engine {
  address: String,
  finish: Option<oneshot::Sender<()>>,
  task: tokio::task::JoinHandle<()>,
}

impl Drop for Engine {
  fn drop(&mut self) {
    self.task.abort();
  }
}

/// Synthetic NDJSON responses, not a captured registry download.
/// Reference: Docker Engine API v1.51, ImageCreate:
/// https://docs.docker.com/reference/api/engine/version/v1.51/#tag/Image/operation/ImageCreate
async fn engine(actor_error: bool) -> Engine {
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let address = format!("tcp://{}", listener.local_addr().unwrap());
  let (finish, receive) = oneshot::channel();
  let task = tokio::spawn(async move {
    let mut receive = Some(receive);
    loop {
      let (mut stream, _) = listener.accept().await.unwrap();
      let mut request = Vec::new();
      while !request.windows(4).any(|w| w == b"\r\n\r\n") {
        let mut buffer = [0u8; 1024];
        let read = stream.read(&mut buffer).await.unwrap();
        assert_ne!(read, 0);
        request.extend_from_slice(&buffer[..read]);
      }
      let request = String::from_utf8(request).unwrap();
      let (prefix, suffix) = if request.starts_with("POST ") && request.contains("/images/create?") {
        if request.contains("needy-actor") {
          (
            concat!(
              "{\"status\":\"Already exists\",\"id\":\"cached-layer\"}\n",
              "{\"status\":\"Downloading\",\"id\":\"actor-layer\",\"progress\":\"1 KiB/4 KiB\",",
              "\"progressDetail\":{\"current\":1024,\"total\":4096}}\n"
            ),
            if actor_error {
              "{\"errorDetail\":{\"message\":\"registry denied fixture\"},\"error\":\"registry denied fixture\"}\n"
            } else {
              "{\"status\":\"Pull complete\",\"id\":\"actor-layer\"}\n"
            },
          )
        } else {
          assert!(request.contains("needy-executor"), "{request}");
          (
            "{\"status\":\"Downloading\",\"id\":\"executor-layer\",\"progressDetail\":{\"current\":256,\"total\":1024}}\n",
            "{\"status\":\"Pull complete\",\"id\":\"executor-layer\"}\n",
          )
        }
      } else {
        assert!(request.starts_with("GET ") && request.contains("/images/") && request.contains("/json"), "{request}");
        ("", r#"{"Config":{"Labels":{"needy.base-abi":"1"}}}"#)
      };
      let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        prefix.len() + suffix.len()
      );
      stream.write_all(header.as_bytes()).await.unwrap();
      stream.write_all(prefix.as_bytes()).await.unwrap();
      stream.flush().await.unwrap();
      if !prefix.is_empty()
        && let Some(receive) = receive.take()
      {
        // Keep the first pull open until the test observes the CLI's output.
        receive.await.unwrap();
      }
      stream.write_all(suffix.as_bytes()).await.unwrap();
    }
  });
  Engine {
    address,
    finish: Some(finish),
    task,
  }
}

async fn pull(actor_error: bool) -> (ExitStatus, Vec<String>, Option<String>) {
  let mut engine = engine(actor_error).await;
  let home = tempfile::tempdir().unwrap();
  let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_needy"));
  for kind in ["CONFIG", "STATE", "DATA", "RUNTIME"] {
    command.env(format!("NEEDY_{kind}_DIR"), home.path().join(kind));
  }
  let mut child = command
    .args(["image", "pull", "--driver", "linux-container"])
    .env("DOCKER_HOST", &engine.address)
    .env_remove("DOCKER_TLS_VERIFY")
    .env_remove("DOCKER_CERT_PATH")
    .stdout(Stdio::null())
    .stderr(Stdio::piped())
    .kill_on_drop(true)
    .spawn()
    .unwrap();
  let mut stderr = BufReader::new(child.stderr.take().unwrap()).lines();
  let mut lines = Vec::new();
  let live = tokio::time::timeout(Duration::from_secs(5), async {
    while let Some(line) = stderr.next_line().await.unwrap() {
      lines.push(line.clone());
      if line.contains("Downloading") {
        assert!(child.try_wait().unwrap().is_none(), "progress must arrive during the pull");
        return Some(line);
      }
    }
    None
  })
  .await
  .ok()
  .flatten();
  engine.finish.take().unwrap().send(()).unwrap();
  let status = tokio::time::timeout(Duration::from_secs(5), async {
    while let Some(line) = stderr.next_line().await.unwrap() {
      lines.push(line);
    }
    child.wait().await.unwrap()
  })
  .await
  .expect("the completed image pull must exit");
  (status, lines, live)
}

// ROOT CAUSE:
//
// Container image pulls showed only their initial message because the engine
// collected and discarded the progress stream before the CLI could render it.
// Progress must reach stderr while the engine is still downloading.
#[tokio::test]
async fn image_pull_shows_layer_progress_before_the_download_finishes() {
  let (status, lines, live) = pull(false).await;

  assert!(live.as_deref().is_some_and(|line| line.contains("actor-layer") && line.contains("1024/4096 bytes")), "{lines:#?}");
  assert!(status.success(), "{lines:#?}");
  assert!(lines.iter().any(|line| line.contains("cached-layer") && line.contains("Already exists")), "{lines:#?}");
  assert!(lines.iter().any(|line| line.contains("executor-layer") && line.contains("256/1024 bytes")), "{lines:#?}");
  assert!(lines.iter().any(|line| line.contains("actor-layer") && line.contains("Pull complete")), "{lines:#?}");
  assert!(lines.iter().any(|line| line.contains("executor-layer") && line.contains("Pull complete")), "{lines:#?}");
}

#[tokio::test]
async fn image_pull_keeps_progress_and_fails_on_an_engine_stream_error() {
  let (status, lines, live) = pull(true).await;

  assert!(live.is_some(), "{lines:#?}");
  assert!(!status.success(), "{lines:#?}");
  assert!(lines.iter().any(|line| line.contains("registry denied fixture")), "{lines:#?}");
  assert!(!lines.iter().any(|line| line.contains("linux-container:") && line.contains("base ABI")), "{lines:#?}");
  assert!(lines.iter().any(|line| line.contains("executor-layer") && line.contains("Pull complete")), "{lines:#?}");
}
