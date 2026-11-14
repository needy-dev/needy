use super::*;

fn answer(status_code: u16) -> BollardError {
  BollardError::DockerResponseServerError {
    status_code,
    message: "no such object".into(),
  }
}

#[test]
fn the_engines_404_is_not_found_and_any_other_answer_is_refused() {
  assert!(Error::from(answer(404)).is_not_found());
  assert_eq!(Error::from(answer(500)), Error::Refused("Docker responded with status code 500: no such object".into()));
  assert_eq!(Error::from(answer(500)).to_string(), "docker: Docker responded with status code 500: no such object");
}

#[test]
fn a_stop_of_a_stopped_container_and_a_removal_of_a_gone_one_are_fine() {
  assert_eq!(ignore(Err(answer(304)), &[304]), Ok(()));
  assert_eq!(ignore(Err(answer(404)), &[404, 409]), Ok(()));
  assert_eq!(ignore(Err(answer(409)), &[404, 409]), Ok(()));
  assert!(ignore(Err(answer(404)), &[304]).unwrap_err().is_not_found());
}

#[test]
fn a_wait_answers_the_exit_code_also_when_bollard_reports_it_as_an_error() {
  let exited = models::ContainerWaitResponse {
    status_code: 0,
    ..Default::default()
  };
  assert_eq!(exit_code("c", Some(Ok(exited))), Ok(0));
  let failed = BollardError::DockerContainerWaitError {
    error: String::new(),
    code: 137,
  };
  assert_eq!(exit_code("c", Some(Err(failed))), Ok(137));
  assert_eq!(exit_code("c", None), Err(Error::Refused("the wait for c ended without its exit".into())));
}

#[test]
fn a_bare_image_name_is_pulled_at_its_latest_tag() {
  assert_eq!(with_tag("ubuntu"), "ubuntu:latest");
  assert_eq!(with_tag("ghcr.io/example/needy-executor"), "ghcr.io/example/needy-executor:latest");
  assert_eq!(with_tag("localhost:5000/needy"), "localhost:5000/needy:latest");
  assert_eq!(with_tag("needy-executor:v1"), "needy-executor:v1");
  assert_eq!(with_tag("ubuntu@sha256:0123"), "ubuntu@sha256:0123");
}

#[tokio::test]
async fn an_unreachable_engine_fails_every_use_and_the_probe_with_the_reason() {
  let engine: Arc<dyn Engine> = Arc::new(Unreachable("socket missing".into()));

  assert_eq!(engine.remove("c").await.unwrap_err().to_string(), "no Docker-compatible engine answers: socket missing");
  assert_eq!(engine.pull("image").await.unwrap_err().to_string(), "no Docker-compatible engine answers: socket missing");
  assert_eq!(detect(engine).await, Err("no Docker-compatible engine answers: socket missing".into()));
}

#[test]
fn a_containers_labels_name_its_run_and_its_role() {
  let labels = labels("0123456789ab", "executor");

  assert_eq!(labels[RUN_LABEL], "0123456789ab");
  assert_eq!(labels[ROLE_LABEL], "executor");
}

// ROOT CAUSE:
//
// A runtime service restart closes the wait connection while its container
// can remain alive. That transport error must trigger a resource probe;
// returning it as an actor exit destroys surviving work at the control layer.
#[tokio::test]
async fn a_runtime_wait_disconnect_probes_the_existing_container_and_waits_again() {
  use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
  let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await.unwrap();
  let address = format!("http://localhost:{}", listener.local_addr().unwrap().port());
  let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
  let observed = requests.clone();
  let service = tokio::spawn(async move {
    // Source: https://github.com/moby/moby/blob/v28.5.1/api/swagger.yaml
    // Reference: Docker Engine API, ContainerInspect / ContainerWait.
    // Synthetic values select an existing running resource, then exit 42.
    let replies = [
      None,
      Some(r#"{"State":{"Running":true,"ExitCode":0}}"#),
      Some(r#"{"StatusCode":42}"#),
    ];
    for reply in replies {
      let (mut stream, _) = listener.accept().await.unwrap();
      let mut header = Vec::new();
      while !header.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        stream.read_exact(&mut byte).await.unwrap();
        header.push(byte[0]);
      }
      observed.lock().unwrap().push(String::from_utf8(header).unwrap().lines().next().unwrap().to_string());
      if let Some(body) = reply {
        let response =
          format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        stream.write_all(response.as_bytes()).await.unwrap();
      }
    }
  });
  let docker = bollard::Docker::connect_with_http(&address, 5, bollard::API_DEFAULT_VERSION).unwrap();
  let engine = Docker(docker);
  let result = tokio::time::timeout(Duration::from_secs(5), engine.wait("actor")).await.unwrap();
  service.abort();
  assert_eq!(result.unwrap(), 42);
  let requests = requests.lock().unwrap();
  assert_eq!(requests.len(), 3);
  assert!(requests[0].contains("/containers/actor/wait?"));
  assert!(requests[1].contains("/containers/actor/json"));
  assert!(requests[2].contains("/containers/actor/wait?"));
}
