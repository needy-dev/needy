//! Exec streams run to completion against an in-process executor.

#![cfg(unix)]

mod support;

use needy_api_client::exec::{ExecStream, Output};
use needy_api_client::{Error, Kind};
use needy_api_proto::v1::ExecStart;
use needy_common::process::Exit;

fn start(argv: &[&str]) -> ExecStart {
  ExecStart {
    argv: argv.iter().map(|a| a.to_string()).collect(),
    ..Default::default()
  }
}

#[tokio::test]
async fn a_run_returns_the_output_apart_and_the_exit_code() {
  let executor = support::executor().await;
  let stream = ExecStream::start(&executor.client, start(&["sh", "-c", "echo out; echo err >&2; exit 7"])).await.unwrap();

  let output = stream.run(None).await.unwrap();

  assert_eq!(
    output,
    Output {
      stdout: b"out\n".to_vec(),
      stderr: b"err\n".to_vec(),
      exit: Exit {
        code: Some(7),
        signal: String::new()
      },
    }
  );
}

#[tokio::test]
async fn a_run_returns_a_signal_death_by_name() {
  let executor = support::executor().await;
  let stream = ExecStream::start(&executor.client, start(&["sh", "-c", "kill -TERM $$"])).await.unwrap();

  let output = stream.run(None).await.unwrap();

  assert_eq!(
    output.exit,
    Exit {
      code: None,
      signal: "SIGTERM".into()
    }
  );
}

#[tokio::test]
async fn a_run_feeds_its_input_and_closes_it() {
  let executor = support::executor().await;
  let stream = ExecStream::start(&executor.client, start(&["wc", "-c"])).await.unwrap();

  let output = stream.run(Some(vec![b'x'; 3 << 20])).await.unwrap();

  assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), (3 << 20).to_string());
  assert_eq!(output.exit.code, Some(0));
}

#[tokio::test]
async fn a_command_that_cannot_start_fails_to_start() {
  let executor = support::executor().await;

  let error = ExecStream::start(&executor.client, start(&["/nonexistent/needy-test-command"])).await.err().unwrap();

  assert_eq!(error.kind(), Kind::Failed);
  assert!(error.to_string().starts_with("cannot start /nonexistent/needy-test-command"), "{error}");
  assert!(matches!(error, Error::Status(_)));
}
