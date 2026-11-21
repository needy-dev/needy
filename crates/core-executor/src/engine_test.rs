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
  let expected = "no Docker-compatible engine answers: socket missing";

  assert_eq!(engine.remove("c").await.unwrap_err().to_string(), expected);
  assert_eq!(engine.pull("image").await.unwrap_err().to_string(), expected);
  assert_eq!(engine.commit_container("c", "needy/snap", "v1").await.unwrap_err().to_string(), expected);
  let exported: Result<Vec<_>, _> = engine.export_image("needy/snap:v1").try_collect().await;
  assert_eq!(exported.unwrap_err().to_string(), expected);
  assert_eq!(engine.load_image(futures_util::stream::empty().boxed()).await.unwrap_err().to_string(), expected);
  assert_eq!(engine.remove_image("needy/snap:v1").await.unwrap_err().to_string(), expected);
  assert_eq!(detect(engine).await, Err(expected.into()));
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

/// Reads one HTTP/1.1 request's header block and decoded body (supporting
/// `Content-Length` and `Transfer-Encoding: chunked`) from `stream`.
async fn read_http_request(stream: &mut tokio::net::TcpStream) -> (String, Vec<u8>) {
  use tokio::io::AsyncReadExt as _;
  let mut header = Vec::new();
  while !header.ends_with(b"\r\n\r\n") {
    let mut byte = [0];
    stream.read_exact(&mut byte).await.unwrap();
    header.push(byte[0]);
  }
  let header = String::from_utf8(header).unwrap();
  let mut body = Vec::new();
  let is_chunked = header.lines().any(|line| {
    let lower = line.to_ascii_lowercase();
    lower.starts_with("transfer-encoding:") && lower.contains("chunked")
  });
  if is_chunked {
    loop {
      let mut size_line = Vec::new();
      while !size_line.ends_with(b"\r\n") {
        let mut byte = [0];
        stream.read_exact(&mut byte).await.unwrap();
        size_line.push(byte[0]);
      }
      let size_str = std::str::from_utf8(&size_line[..size_line.len() - 2]).unwrap().trim();
      let size = usize::from_str_radix(size_str, 16).unwrap();
      if size == 0 {
        let mut trailer = [0; 2];
        stream.read_exact(&mut trailer).await.unwrap();
        break;
      }
      let start = body.len();
      body.resize(start + size, 0);
      stream.read_exact(&mut body[start..]).await.unwrap();
      let mut crlf = [0; 2];
      stream.read_exact(&mut crlf).await.unwrap();
    }
  } else if let Some(len) = header.lines().find_map(|line| {
    let (name, value) = line.split_once(':')?;
    name.eq_ignore_ascii_case("content-length").then(|| value.trim().parse::<usize>().unwrap())
  }) {
    body.resize(len, 0);
    stream.read_exact(&mut body).await.unwrap();
  }
  (header, body)
}

#[tokio::test]
async fn commit_export_load_and_remove_image_speak_the_docker_engine_api() {
  use tokio::io::AsyncWriteExt as _;
  let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await.unwrap();
  let address = format!("http://localhost:{}", listener.local_addr().unwrap().port());
  let observed = Arc::new(std::sync::Mutex::new(Vec::<(String, Vec<u8>)>::new()));
  let recorded = observed.clone();
  let service = tokio::spawn(async move {
    // Source: https://github.com/moby/moby/blob/v28.5.1/api/swagger.yaml
    // Reference: Docker Engine API, ImageCommit (`POST /commit`), ImageGet
    // (`GET /images/{name}/get`), ImageLoad (`POST /images/load`), and
    // ImageDelete (`DELETE /images/{name}`). Synthetic payloads test our
    // request paths, headers, streaming bodies, and status/error handling.
    let responses: [(&str, &str, &[u8]); 6] = [
      ("201 Created", "application/json", br#"{"Id":"sha256:deadbeef"}"#),
      ("200 OK", "application/x-tar", b"tar-part-1tar-part-2"),
      ("200 OK", "application/json", br#"{"stream":"Loaded image: needy/snap:v1\n"}"#),
      ("200 OK", "application/json", br#"{"errorDetail":{"message":"invalid tar header"}}"#),
      ("200 OK", "application/json", br#"[{"Deleted":"sha256:deadbeef"}]"#),
      ("404 Not Found", "application/json", br#"{"message":"No such image: missing:v1"}"#),
    ];
    for (status, content_type, body) in responses {
      let (mut stream, _) = listener.accept().await.unwrap();
      let req = read_http_request(&mut stream).await;
      recorded.lock().unwrap().push(req);
      let head = format!("HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
      stream.write_all(head.as_bytes()).await.unwrap();
      stream.write_all(body).await.unwrap();
    }
  });

  let docker = bollard::Docker::connect_with_http(&address, 5, bollard::API_DEFAULT_VERSION).unwrap();
  let engine = Docker(docker);

  let id = engine.commit_container("needy-edit-1", "needy/snap", "v1").await.unwrap();
  assert_eq!(id, "sha256:deadbeef");

  let exported: Vec<bytes::Bytes> = engine.export_image("needy/snap:v1").try_collect().await.unwrap();
  assert_eq!(exported.concat(), b"tar-part-1tar-part-2");

  let tar = futures_util::stream::iter([
    Ok(bytes::Bytes::from_static(b"chunk-a")),
    Ok(bytes::Bytes::from_static(b"chunk-b")),
  ])
  .boxed();
  engine.load_image(tar).await.unwrap();

  let bad_tar = futures_util::stream::iter([Ok(bytes::Bytes::from_static(b"corrupt"))]).boxed();
  let load_err = engine.load_image(bad_tar).await.unwrap_err();
  assert!(matches!(load_err, Error::Refused(ref msg) if msg.contains("invalid tar header")), "{load_err:?}");

  engine.remove_image("needy/snap:v1").await.unwrap();
  engine.remove_image("missing:v1").await.unwrap();

  service.await.unwrap();
  let observed = observed.lock().unwrap();
  assert_eq!(observed.len(), 6);
  let line = |i: usize| observed[i].0.lines().next().unwrap();
  assert!(line(0).starts_with("POST ") && line(0).contains("/commit?"), "{}", line(0));
  assert!(line(0).contains("container=needy-edit-1"), "{}", line(0));
  assert!(line(0).contains("repo=needy%2Fsnap") || line(0).contains("repo=needy/snap"), "{}", line(0));
  assert!(line(0).contains("tag=v1"), "{}", line(0));
  assert!(line(1).starts_with("GET ") && line(1).contains("/images/needy/snap:v1/get"), "{}", line(1));
  assert!(line(2).starts_with("POST ") && line(2).contains("/images/load?quiet=true"), "{}", line(2));
  assert!(observed[2].0.to_ascii_lowercase().contains("content-type: application/x-tar"), "{}", observed[2].0);
  assert_eq!(observed[2].1, b"chunk-achunk-b");
  assert!(line(4).starts_with("DELETE ") && line(4).contains("/images/needy/snap:v1?force=true"), "{}", line(4));
  assert!(line(5).starts_with("DELETE ") && line(5).contains("/images/missing:v1?force=true"), "{}", line(5));
}
