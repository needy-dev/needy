use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Empty, Full};
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use needy_config::value::Glob;
use needy_core::chaos::Injector;
use needy_core::event::{Event, NetConnect, NetDeny, NetRequest};
use needy_core::network::Policy;
use needy_core::types::{Effect, Fired, Network};
use needy_trace::Dispatch;
use needy_trace::sink::memory::Memory;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UnixListener, UnixStream};
use tokio_stream::wrappers::UnixListenerStream;
use tokio_util::sync::CancellationToken;

use super::*;

/// What the test upstream saw of one request.
#[derive(Debug, Clone, PartialEq)]
struct Seen {
  path: String,
  authorization: Option<String>,
}

/// An HTTP server on loopback that answers 200 and records each request.
async fn upstream() -> (SocketAddr, Arc<Mutex<Vec<Seen>>>) {
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let address = listener.local_addr().unwrap();
  let seen = Arc::new(Mutex::new(Vec::new()));
  let record = seen.clone();
  tokio::spawn(async move {
    while let Ok((stream, _)) = listener.accept().await {
      let record = record.clone();
      tokio::spawn(async move {
        let service = hyper::service::service_fn(move |request: Request<hyper::body::Incoming>| {
          record.lock().unwrap().push(Seen {
            path: request.uri().path().to_string(),
            authorization: request.headers().get("authorization").map(|v| v.to_str().unwrap().to_string()),
          });
          async { Ok::<_, std::convert::Infallible>(Response::new(Full::new(Bytes::from("upstream")))) }
        });
        // The test client may close first.
        let _ = hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(stream), service).await;
      });
    }
  });
  (address, seen)
}

struct Running {
  socket: std::path::PathBuf,
  trace: Dispatch,
  memory: Memory,
  cancel: CancellationToken,
  task: tokio::task::JoinHandle<()>,
  _dir: tempfile::TempDir,
}

impl Running {
  /// The events the gateway recorded so far.
  async fn events(&self) -> Vec<Event> {
    self.trace.flush().await;
    self.memory.events().iter().map(|e| Event::decode(&e.kind, e.version, e.payload.get()).unwrap()).collect()
  }
}

/// A gateway that allows `127.0.0.1`, maps `model.needy` to `upstream` and
/// injects a header for it, with the faults in `chaos`.
fn start(upstream: SocketAddr, chaos: &str) -> Running {
  let policy = Policy {
    allow: vec![Glob::new("127.0.0.1")],
    upstream: BTreeMap::from([("model.needy".to_string(), format!("http://{upstream}"))]),
    inject: BTreeMap::from([("model.needy".to_string(), ("authorization".to_string(), "Bearer test-secret".to_string()))]),
  };
  let memory = Memory::new();
  let trace = Dispatch::new(
    "s1",
    vec![Box::new(memory.clone())],
    needy_trace::Options {
      capacity: 64,
      version: needy_core::event::SCHEMA_VERSION,
      timing: false,
    },
  );
  let injector = (!chaos.is_empty()).then(|| Arc::new(Injector::new(&serde_yaml::from_str(chaos).unwrap())));
  let cancel = CancellationToken::new();
  let gateway = Arc::new(Gateway::new(policy, trace.clone(), Network::Actor, injector, cancel.clone()));
  let dir = tempfile::tempdir().unwrap();
  let socket = dir.path().join("gateway.sock");
  let listener = UnixListener::bind(&socket).unwrap();
  let task = tokio::spawn(gateway.run(UnixListenerStream::new(listener)));
  Running {
    socket,
    trace,
    memory,
    cancel,
    task,
    _dir: dir,
  }
}

/// Sends `GET uri` in absolute form, as a client behind `HTTP_PROXY` does.
async fn get(socket: &Path, uri: &str) -> (StatusCode, String) {
  let stream = UnixStream::connect(socket).await.unwrap();
  let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream)).await.unwrap();
  tokio::spawn(connection);
  let host = uri.parse::<hyper::Uri>().unwrap().host().unwrap().to_string();
  let request = Request::get(uri).header(hyper::header::HOST, host).body(Empty::<Bytes>::new()).unwrap();
  let response = sender.send_request(request).await.unwrap();
  let status = response.status();
  let body = response.into_body().collect().await.unwrap().to_bytes();
  (status, String::from_utf8_lossy(&body).into_owned())
}

/// Sends `CONNECT authority` and returns the status and the open stream.
async fn connect(socket: &Path, authority: &str) -> (u16, UnixStream) {
  let mut stream = UnixStream::connect(socket).await.unwrap();
  stream.write_all(format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n\r\n").as_bytes()).await.unwrap();
  let mut head = Vec::new();
  while !head.ends_with(b"\r\n\r\n") {
    let mut byte = [0u8; 1];
    assert_eq!(stream.read(&mut byte).await.unwrap(), 1, "the gateway closed during {:?}", String::from_utf8_lossy(&head));
    head.push(byte[0]);
  }
  let status = String::from_utf8_lossy(&head).split(' ').nth(1).unwrap().parse().unwrap();
  (status, stream)
}

#[tokio::test]
async fn a_mapped_host_is_forwarded_to_its_upstream_with_the_injected_header() {
  let (address, seen) = upstream().await;
  let gateway = start(address, "");

  let (status, body) = get(&gateway.socket, "http://model.needy/v1/models").await;

  assert_eq!((status, body.as_str()), (StatusCode::OK, "upstream"));
  assert_eq!(
    *seen.lock().unwrap(),
    [Seen {
      path: "/v1/models".into(),
      authorization: Some("Bearer test-secret".into()),
    }]
  );
  let events = gateway.events().await;
  let [Event::NetRequest(request)] = &events[..] else {
    panic!("{events:?}");
  };
  assert_eq!(
    *request,
    NetRequest {
      network: Network::Actor,
      host: "model.needy".into(),
      path: "/v1/models".into(),
      status: 200,
      ms: request.ms,
    }
  );
}

#[tokio::test]
async fn an_allowed_host_gets_no_header_injected_for_another_host() {
  let (address, seen) = upstream().await;
  let gateway = start(address, "");

  let (status, _) = get(&gateway.socket, &format!("http://{address}/plain")).await;

  assert_eq!(status, StatusCode::OK);
  assert_eq!(
    *seen.lock().unwrap(),
    [Seen {
      path: "/plain".into(),
      authorization: None,
    }]
  );
}

#[tokio::test]
async fn a_host_off_the_allowlist_is_denied() {
  let (address, seen) = upstream().await;
  let gateway = start(address, "");

  let (status, body) = get(&gateway.socket, "http://denied.example.com/secret").await;

  assert_eq!((status, body.as_str()), (StatusCode::FORBIDDEN, "needy: denied.example.com is not allowed\n"));
  assert!(seen.lock().unwrap().is_empty());
  assert_eq!(
    gateway.events().await,
    [Event::NetDeny(NetDeny {
      network: Network::Actor,
      host: "denied.example.com".into(),
      path: Some("/secret".into()),
    })]
  );
}

#[tokio::test]
async fn a_net_fault_fails_an_allowed_request_before_its_upstream() {
  let (address, seen) = upstream().await;
  let gateway = start(address, "faults: [{on: net.connect, match: model.needy, errno: ECONNRESET}]");

  let (status, body) = get(&gateway.socket, "http://model.needy/v1/models").await;

  assert_eq!((status, body.as_str()), (StatusCode::BAD_GATEWAY, "needy chaos: ECONNRESET for model.needy\n"));
  assert!(seen.lock().unwrap().is_empty());
  assert_eq!(
    gateway.events().await,
    [Event::FaultInjected(Fired {
      kind: needy_config::profile::FaultKind::NetConnect,
      target: "model.needy".into(),
      effect: Effect {
        errno: Some("ECONNRESET".into()),
        ..Effect::default()
      },
    })]
  );
}

#[tokio::test]
async fn chaos_does_not_count_a_host_the_allowlist_denied() {
  let (address, _) = upstream().await;
  let gateway = start(address, "faults: [{on: net.connect, nth: 1, errno: ECONNRESET}]");

  let (denied, _) = get(&gateway.socket, "http://denied.example.com/").await;
  let (faulted, _) = get(&gateway.socket, "http://model.needy/").await;

  assert_eq!((denied, faulted), (StatusCode::FORBIDDEN, StatusCode::BAD_GATEWAY));
  let kinds: Vec<&str> = gateway.events().await.iter().map(|e| e.kind().as_str()).collect();
  assert_eq!(kinds, ["net.deny", "fault.injected"]);
}

#[tokio::test]
async fn a_connect_to_an_allowed_host_opens_a_tunnel() {
  let (address, seen) = upstream().await;
  let gateway = start(address, "");

  let (status, mut tunnel) = connect(&gateway.socket, &address.to_string()).await;
  tunnel.write_all(b"GET /tunneled HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").await.unwrap();
  let mut response = String::new();
  tunnel.read_to_string(&mut response).await.unwrap();

  assert_eq!(status, 200);
  assert!(response.starts_with("HTTP/1.1 200"), "{response}");
  assert_eq!(seen.lock().unwrap()[0].path, "/tunneled");
  assert_eq!(
    gateway.events().await,
    [Event::NetConnect(NetConnect {
      network: Network::Actor,
      host: "127.0.0.1".into(),
      authority: address.to_string(),
    })]
  );
}

#[tokio::test]
async fn a_connect_to_a_host_off_the_allowlist_is_denied() {
  let (address, _) = upstream().await;
  let gateway = start(address, "");

  let (status, _) = connect(&gateway.socket, "denied.example.com:443").await;

  assert_eq!(status, 403);
  assert_eq!(
    gateway.events().await,
    [Event::NetDeny(NetDeny {
      network: Network::Actor,
      host: "denied.example.com".into(),
      path: None,
    })]
  );
}

#[tokio::test]
async fn cancelling_the_owner_ends_the_gateway_and_its_open_tunnels() {
  let (address, _) = upstream().await;
  let gateway = start(address, "");
  let (status, mut tunnel) = connect(&gateway.socket, &address.to_string()).await;
  assert_eq!(status, 200);

  gateway.cancel.cancel();

  tokio::time::timeout(Duration::from_secs(5), gateway.task).await.expect("the gateway ends").unwrap();
  let mut rest = Vec::new();
  let read = tokio::time::timeout(Duration::from_secs(5), tunnel.read_to_end(&mut rest)).await.expect("the tunnel closes");
  // A closed tunnel reads end of file, or a reset.
  assert!(!matches!(read, Ok(n) if n > 0), "{rest:?}");
}
