use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Empty, Full};
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use needy_config::profile::FaultDomain;
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

/// A gateway that allows `127.0.0.1` and has the service `model`
/// (`model.needy`) at `upstream`, with the faults in `chaos`.
fn start(upstream: SocketAddr, chaos: &str) -> Running {
  let network = serde_yaml::from_str(&format!("{{ allow: [127.0.0.1], services: {{ model: {{ url: \"http://{upstream}\" }} }} }}")).unwrap();
  let policy = Policy::actor(&network);
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
async fn a_service_s_host_is_forwarded_to_its_url_as_sent() {
  let (address, seen) = upstream().await;
  let gateway = start(address, "");

  let (status, body) = get(&gateway.socket, "http://model.needy/v1/models").await;

  assert_eq!((status, body.as_str()), (StatusCode::OK, "upstream"));
  // TODO(mitm): no header is added; header injection was removed.
  assert_eq!(
    *seen.lock().unwrap(),
    [Seen {
      path: "/v1/models".into(),
      authorization: None,
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
async fn an_allowed_host_is_forwarded_to_itself() {
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
async fn a_refuse_fault_answers_as_a_refused_upstream_before_reaching_it() {
  let (address, seen) = upstream().await;
  let gateway = start(address, "faults: { down: {net: {connect: model.needy}, refuse: true} }");

  let (status, body) = get(&gateway.socket, "http://model.needy/v1/models").await;

  assert_eq!((status, body.as_str()), (StatusCode::BAD_GATEWAY, "needy: model.needy: connection refused (needy chaos: down)\n"));
  assert!(seen.lock().unwrap().is_empty());
  assert_eq!(
    gateway.events().await,
    [Event::FaultInjected(Fired {
      name: "down".into(),
      domain: FaultDomain::Net,
      target: "model.needy".into(),
      effect: Effect {
        refuse: true,
        ..Effect::default()
      },
    })]
  );
}

#[tokio::test]
async fn a_status_fault_answers_a_service_request_with_its_status() {
  let (address, seen) = upstream().await;
  let gateway = start(address, "faults: { busy: {net: {request: model.needy}, status: 503} }");

  let (status, body) = get(&gateway.socket, "http://model.needy/v1/models").await;

  assert_eq!((status, body.as_str()), (StatusCode::SERVICE_UNAVAILABLE, "needy chaos: busy\n"));
  assert!(seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_delay_fault_holds_the_request_then_lets_it_through() {
  let (address, seen) = upstream().await;
  let gateway = start(address, "faults: { slow: {net: {connect: model.needy}, delay: 300ms} }");

  let started = std::time::Instant::now();
  let (status, _) = get(&gateway.socket, "http://model.needy/").await;

  assert_eq!(status, StatusCode::OK);
  assert!(started.elapsed() >= Duration::from_millis(300), "{:?}", started.elapsed());
  assert_eq!(seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_reset_fault_closes_the_connection_without_an_answer() {
  let (address, _) = upstream().await;
  let gateway = start(address, "faults: { cut: {net: {connect: '*'}, reset: true} }");

  for request in [
    "GET http://model.needy/ HTTP/1.1\r\nHost: model.needy\r\n\r\n",
    "CONNECT 127.0.0.1:443 HTTP/1.1\r\nHost: 127.0.0.1:443\r\n\r\n",
  ] {
    let mut stream = UnixStream::connect(&gateway.socket).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut answer = Vec::new();
    let read = tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut answer)).await.expect("the gateway closes");
    assert!(answer.is_empty(), "{request}: {:?}", String::from_utf8_lossy(&answer));
    // End of file, or a reset.
    assert!(!matches!(read, Ok(n) if n > 0));
  }
}

#[tokio::test]
async fn a_hang_fault_holds_the_tunnel_until_the_owner_cancels() {
  let (address, _) = upstream().await;
  let gateway = start(address, "faults: { stuck: {net: {connect: '127.0.0.1'}, hang: true} }");
  let mut stream = UnixStream::connect(&gateway.socket).await.unwrap();
  stream.write_all(format!("CONNECT {address} HTTP/1.1\r\nHost: {address}\r\n\r\n").as_bytes()).await.unwrap();

  let mut byte = [0u8; 1];
  let waited = tokio::time::timeout(Duration::from_millis(300), stream.read(&mut byte)).await;
  assert!(waited.is_err(), "the gateway answered a hanging CONNECT");

  gateway.cancel.cancel();
  tokio::time::timeout(Duration::from_secs(5), gateway.task).await.expect("the gateway ends").unwrap();
}

#[tokio::test]
async fn chaos_does_not_count_a_host_the_allowlist_denied() {
  let (address, _) = upstream().await;
  let gateway = start(address, "faults: { first: {net: {connect: '*'}, nth: 1, refuse: true} }");

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
