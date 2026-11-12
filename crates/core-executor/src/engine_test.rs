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
  assert_eq!(detect(engine).await, Err("no Docker-compatible engine answers: socket missing".into()));
}

#[test]
fn a_containers_labels_name_its_run_and_its_role() {
  let labels = labels("0123456789ab", "executor");

  assert_eq!(labels[RUN_LABEL], "0123456789ab");
  assert_eq!(labels[ROLE_LABEL], "executor");
}
