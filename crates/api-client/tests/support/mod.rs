//! An in-process executor for the client's tests: `ExecutorService` serving
//! `ProcessService` on a socket in a temporary directory.

use needy_api_client::Client;
use needy_api_proto::v1::process_service_server::ProcessServiceServer;
use needy_core_executor::process::Host;
use needy_core_executor::service::ExecutorService;
use tokio_stream::wrappers::UnixListenerStream;

pub struct Executor {
  // NOTICE: each test file compiles this module on its own, and not every
  // file uses the directory.
  #[allow(dead_code)]
  pub dir: tempfile::TempDir,
  /// The executor's processes; shutting it down ends every stream.
  #[allow(dead_code)]
  pub host: Host,
  pub client: Client,
}

pub async fn executor() -> Executor {
  let dir = tempfile::tempdir().unwrap();
  let socket = dir.path().join("executor.sock");
  let listener = tokio::net::UnixListener::bind(&socket).unwrap();
  let host = Host::default();
  let service = ExecutorService {
    host: host.clone(),
    name: "test".into(),
  };
  tokio::spawn(
    tonic::transport::Server::builder()
      .add_service(ProcessServiceServer::new(service))
      .serve_with_incoming(UnixListenerStream::new(listener)),
  );
  let client = Client::connect(&format!("unix:{}", socket.display())).await.unwrap();
  Executor { dir, host, client }
}
