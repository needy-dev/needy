//! A server-side response fault at the real CLI boundary. The handler creates
//! the invocation, then the transport reports failure to its creating client.

use super::*;
use needy_api_client::proto::v1::{
  process_service_server::ProcessServiceServer, run_service_server::RunServiceServer, runner_service_server::RunnerServiceServer,
};
use needy_api_server::protocol::grpc::v1::{process, run, runner};
use needy_api_server::{
  control::Caller,
  middleware::authentication::{AuthLayer, Credentials, Token},
};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

#[derive(Clone)]
struct LoseCreateReply<S>(S);
impl<S> tower::Service<http::Request<tonic::body::Body>> for LoseCreateReply<S>
where
  S: tower::Service<http::Request<tonic::body::Body>, Response = http::Response<tonic::body::Body>> + Send + 'static,
  S::Future: Send + 'static,
{
  type Response = S::Response;
  type Error = S::Error;
  type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;
  fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
    self.0.poll_ready(cx)
  }
  fn call(&mut self, request: http::Request<tonic::body::Body>) -> Self::Future {
    let lose = request.uri().path() == "/needy.v1.RunService/CreateRun";
    let handled = self.0.call(request);
    Box::pin(async move {
      let response = handled.await?;
      Ok(if lose {
        tonic::Status::unavailable("creation response was lost after admission").into_http()
      } else {
        response
      })
    })
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_lost_create_reply_keeps_the_bound_execution_holder() {
  let home = Home::new();
  stand_in_actor(&home);
  let daemon = Arc::new(
    needy_daemon::Daemon::start(needy_daemon::Config {
      state_dir: home.path().join("state"),
      runtime_dir: home.path().join("runtime"),
      actor_image: home.path().join("image"),
      vm_images: home.path().join("images"),
      kvm: home.path().join("kvm"),
      guest_needy: Some(home.path().join("needy-linux")),
      guest_disks: home.path().join("guest"),
      executor_image: "example.com/executor:trial".into(),
      heartbeat_interval: Duration::from_secs(1),
      local_runner: None,
      report_wait: needy_daemon::REPORT_WAIT,
      mirror_out_wait: needy_daemon::MIRROR_OUT_WAIT,
    })
    .await
    .unwrap(),
  );
  let socket = home.path().join("control.sock");
  let listener = tokio::net::UnixListener::bind(&socket).unwrap();
  let stop = tokio::sync::watch::channel(false);
  let mut ending = stop.1;
  let server = tonic::transport::Server::builder()
    .layer(AuthLayer::new(Credentials {
      allow_anonymous: true,
      listener: Caller::Operator,
      token: Token(None),
      runners: Some(daemon.clone()),
      clients: None,
    }))
    .layer(tower::layer::layer_fn(LoseCreateReply))
    .add_service(RunServiceServer::new(run::Adapter {
      runs: daemon.clone(),
      caller: Caller::Operator,
    }))
    .add_service(RunnerServiceServer::new(runner::Adapter {
      runners: daemon.clone(),
      caller: Caller::Operator,
    }))
    .add_service(ProcessServiceServer::new(process::Adapter {
      processes: daemon.clone(),
      caller: Caller::Operator,
    }))
    .serve_with_incoming_shutdown(tokio_stream::wrappers::UnixListenerStream::new(listener), async move {
      ending.changed().await.ok();
    });
  let serving = tokio::spawn(server);
  let profile = home.write("client.yaml", "actor: {driver: linux-firecracker}\nexecutors:\n  workstation: {driver: native, runner: {client: true}}\ncommands:\n  - {match: '**', to: workstation}\n");
  let endpoint = format!("unix:{}", socket.display());
  let created = home.run(&[
    "--address",
    &endpoint,
    "run",
    "create",
    "-f",
    profile.to_str().unwrap(),
  ]);
  assert!(!created.status.success(), "the response fault must reach the CLI");
  assert!(stderr(&created).contains("creation response was lost"), "{}", stderr(&created));
  use needy_api_server::control::{Runs, StopMode};
  let runs = daemon.list().await.unwrap();
  assert_eq!(runs.len(), 1);
  assert_eq!(runs[0].state, needy_core::types::RunState::Running);
  let output = home.run(&[
    "--address",
    &endpoint,
    "run",
    "exec",
    &runs[0].id,
    "--",
    "printf",
    "survived",
  ]);
  assert!(output.status.success(), "a lost response must not kill the bound holder: {}", stderr(&output));
  assert_eq!(stdout(&output), "survived");
  daemon.stop(&runs[0].id, needy_core::types::Principal::Operator, StopMode::Now).await.unwrap();
  daemon.shutdown().await;
  stop.0.send(true).unwrap();
  serving.await.unwrap().unwrap();
}
