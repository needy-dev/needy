use std::path::{Path, PathBuf};

use needy_config::profile::{Docker as DockerConfig, Executor, User};

use super::*;

fn spec(args: &[&str]) -> InstanceSpec {
  InstanceSpec {
    name: "build".into(),
    key: "build@s1".into(),
    session: "s1".into(),
    config: Executor {
      docker: Some(DockerConfig {
        args: args.iter().map(|a| a.to_string()).collect(),
        ..Default::default()
      }),
      ..Default::default()
    },
    base: PathBuf::from("/home/user/profile"),
    user: User::default(),
    hostname: Some("instance-1".into()),
  }
}

fn options() -> Options {
  Options {
    image: "needy-executor:latest".into(),
    state_dir: PathBuf::from("/home/user/state"),
    allow: Vec::new(),
  }
}

fn refusal(args: &[&str], options: &Options) -> String {
  match container(&spec(args), options) {
    Ok(container) => panic!("{args:?} was accepted: {container:?}"),
    Err(e) => e.to_string(),
  }
}

#[test]
fn the_daemon_sets_name_user_hostname_label_mounts_and_command() {
  let container = container(&spec(&[]), &options()).unwrap();

  assert_eq!(container.name, "needy-build-s1");
  assert_eq!(container.socket_dir, Path::new("/home/user/state/executors/build-s1"));
  assert_eq!(container.workspace_dir, Path::new("/home/user/state/workspaces/build-s1"));
  assert_eq!(container.workspace, "/home/agent/workspace");
  let body = &container.body;
  assert_eq!(body.image.as_deref(), Some("needy-executor:latest"));
  assert_eq!(body.user.as_deref(), Some("1000:1000"));
  // The recipe's hostname, so the actor and its executors agree on it.
  assert_eq!(body.hostname.as_deref(), Some("instance-1"));
  assert_eq!(body.labels.as_ref().unwrap()["needy.session"], "s1");
  assert_eq!(
    body.cmd.as_deref().unwrap(),
    [
      "needy",
      "daemon",
      "--mode",
      "executor",
      "--listen",
      "unix:/run/needy/executor.sock",
      "--name",
      "build"
    ]
  );
  let host = body.host_config.as_ref().unwrap();
  assert_eq!(host.auto_remove, Some(true));
  assert_eq!(
    host.binds.as_deref().unwrap(),
    [
      "/home/user/state/workspaces/build-s1:/home/agent/workspace",
      "/home/user/state/executors/build-s1:/run/needy"
    ]
  );
}

#[test]
fn a_profile_image_and_workspace_replace_the_defaults() {
  let mut spec = spec(&[]);
  spec.config.workspace = Some("ws".into());
  spec.config.docker.as_mut().unwrap().image = Some("example.com/toolchain:1".into());
  spec.hostname = None;

  let container = container(&spec, &options()).unwrap();

  assert_eq!(container.workspace_dir, Path::new("/home/user/profile/ws"));
  assert_eq!(container.body.image.as_deref(), Some("example.com/toolchain:1"));
  assert_eq!(container.body.hostname, None);
}

#[test]
fn resource_options_become_limits() {
  let container = container(
    &spec(&[
      "--cpus=1.5",
      "--memory",
      "8g",
      "--pids-limit=512",
      "--shm-size=64m",
      "-e",
      "A=1",
      "--init",
    ]),
    &options(),
  )
  .unwrap();

  let host = container.body.host_config.unwrap();
  assert_eq!(host.nano_cpus, Some(1_500_000_000));
  assert_eq!(host.memory, Some(8 << 30));
  assert_eq!(host.pids_limit, Some(512));
  assert_eq!(host.shm_size, Some(64 << 20));
  assert_eq!(host.init, Some(true));
  assert_eq!(container.body.env.as_deref().unwrap(), ["A=1"]);
}

#[test]
fn profile_mounts_follow_the_daemon_mounts() {
  let container = container(
    &spec(&[
      "-v",
      "/cache:/cache:ro",
      "--mount",
      "type=bind,source=/data,target=/data,readonly",
      "--tmpfs",
      "/tmp:size=64m",
    ]),
    &options(),
  )
  .unwrap();

  let host = container.body.host_config.unwrap();
  assert_eq!(host.binds.unwrap()[2], "/cache:/cache:ro");
  let mount = &host.mounts.unwrap()[0];
  assert_eq!(mount.typ, Some(MountType::BIND));
  assert_eq!((mount.source.as_deref(), mount.target.as_deref(), mount.read_only), (Some("/data"), Some("/data"), Some(true)));
  assert_eq!(host.tmpfs.unwrap()["/tmp"], "size=64m");
}

#[test]
fn fields_the_daemon_owns_are_refused() {
  for args in [
    &["--network", "host"][..],
    &["--name=x"],
    &["--label", "a=b"],
    &["--hostname", "h"],
    &["--user", "0"],
    &["--rm"],
  ] {
    assert!(refusal(args, &options()).ends_with("is set by the daemon"), "{args:?}");
  }
  assert_eq!(
    refusal(&["-v", "/data:/home/agent/workspace"], &options()),
    "the workspace mount at /home/agent/workspace is set by the daemon"
  );
}

#[test]
fn privileged_options_need_the_operator() {
  assert_eq!(refusal(&["--privileged"], &options()), "--privileged is not allowed by the operator");
  assert_eq!(refusal(&["--cap-add", "SYS_ADMIN"], &options()), "--cap-add is not allowed by the operator");
  assert_eq!(refusal(&["--pid", "host"], &options()), "--pid host is not allowed by the operator");
  assert_eq!(
    refusal(&["--security-opt", "seccomp=unconfined"], &options()),
    "--security-opt seccomp=unconfined is not allowed by the operator"
  );
  assert_eq!(refusal(&["-v", "/:/host"], &options()), "mounting the host root is not allowed by the operator");
  assert_eq!(refusal(&["--mount", "type=bind,source=/,target=/h"], &options()), "mounting the host root is not allowed by the operator");
  assert!(container(&spec(&["--pid", "container:x"]), &options()).is_ok());

  let operator = Options {
    allow: ["--privileged", "--pid", "host-root"].map(String::from).to_vec(),
    ..options()
  };
  let host = container(&spec(&["--privileged", "--pid", "host", "-v", "/:/host"]), &operator).unwrap().body.host_config.unwrap();
  assert_eq!((host.privileged, host.pid_mode.as_deref()), (Some(true), Some("host")));
}

#[test]
fn an_unknown_option_is_an_error_not_dropped() {
  assert_eq!(refusal(&["--gpus", "all"], &options()), "--gpus is not a supported docker run option");
  assert_eq!(refusal(&["--memory"], &options()), "--memory needs a value");
  assert_eq!(refusal(&["--memory=lots"], &options()), "--memory lots: expected a size such as 512m or 8g");
}

/// Creates an executor container from the executor image and destroys it.
/// Skips without Docker or without the image (`needy build --executor`), and
/// off Linux, where the host cannot reach a socket inside a container.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn create_and_destroy_an_executor_container() {
  use needy_api_proto::v1::{ExecRequest, ExecStart, exec_request, exec_response};

  let state = tempfile::tempdir().unwrap();
  let options = Options {
    state_dir: state.path().to_path_buf(),
    ..options()
  };
  let provider = Docker::new(options.clone()).unwrap();
  if provider.docker.ping().await.is_err() || provider.docker.inspect_image(&options.image).await.is_err() {
    eprintln!("skipped: no Docker or no {} image", options.image);
    return;
  }
  let mut spec = spec(&["--memory=256m"]);
  spec.key = format!("test-{}", uuid::Uuid::new_v4().simple());
  // The executor runs as the owner of the socket directory, which is the
  // test's user.
  use std::os::unix::fs::MetadataExt;
  let owner = state.path().metadata().unwrap();
  spec.user = User {
    name: "agent".into(),
    uid: owner.uid(),
    gid: owner.gid(),
  };

  let handle = provider.create(&spec).await.unwrap();

  let (tx, rx) = tokio::sync::mpsc::channel(1);
  tx.send(ExecRequest {
    payload: Some(exec_request::Payload::Start(ExecStart {
      argv: vec!["hostname".into()],
      no_stdin: true,
      ..Default::default()
    })),
  })
  .await
  .unwrap();
  let mut responses = handle.client.processes().exec(tokio_stream::wrappers::ReceiverStream::new(rx)).await.unwrap().into_inner();
  let mut stdout = Vec::new();
  while let Some(message) = responses.message().await.unwrap() {
    match message.payload {
      Some(exec_response::Payload::Stdout(bytes)) => stdout.extend(bytes),
      Some(exec_response::Payload::Exited(_)) => break,
      _ => {}
    }
  }
  assert_eq!(String::from_utf8_lossy(&stdout).trim(), "instance-1");
  assert_eq!(handle.runtime, "docker");

  provider.destroy(&handle).await.unwrap();
  let gone = provider.docker.inspect_container(&handle.id, None).await;
  assert!(
    matches!(
      gone,
      Err(DockerError::DockerResponseServerError {
        status_code: 404,
        ..
      })
    ) || gone.is_ok_and(|c| c.state.and_then(|s| s.running) == Some(false)),
    "the container is removed"
  );
}
