use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Empty, Full};
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use needy_config::profile::FaultDomain;
use needy_core::chaos::Injector;
use needy_core::event::{Event, NetConnect, NetDeny, NetError, NetRequest};
use needy_core::network::Policy;
use needy_core::types::{Effect, Fired, Network};
use needy_trace::Dispatch;
use needy_trace::sink::memory::Memory;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UnixListener, UnixStream};
use tokio_stream::wrappers::UnixListenerStream;
use tokio_util::sync::CancellationToken;

use super::testing::{Seen as ProxySeen, Socks5Server};
use super::*;

/// What the test upstream saw of one request.
#[derive(Debug, Clone, PartialEq)]
struct Seen {
  path: String,
  authorization: Option<String>,
  proxy_authorization: Option<String>,
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
            proxy_authorization: request.headers().get("proxy-authorization").map(|v| v.to_str().unwrap().to_string()),
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
  start_with(&format!("{{ allow: [127.0.0.1], services: {{ model: {{ url: \"http://{upstream}\" }} }} }}"), chaos, GatewayOptions::default())
}

fn start_with(network_yaml: &str, chaos: &str, options: GatewayOptions) -> Running {
  let network = serde_yaml::from_str(network_yaml).unwrap();
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
  let gateway = Arc::new(Gateway::with_options(policy, trace.clone(), Network::Actor, injector, cancel.clone(), options));
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
  get_with_proxy_auth(socket, uri, None).await
}

async fn get_with_proxy_auth(socket: &Path, uri: &str, proxy_auth: Option<&str>) -> (StatusCode, String) {
  let stream = UnixStream::connect(socket).await.unwrap();
  let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream)).await.unwrap();
  tokio::spawn(connection);
  let host = uri.parse::<hyper::Uri>().unwrap().host().unwrap().to_string();
  let mut builder = Request::get(uri).header(hyper::header::HOST, host);
  if let Some(auth) = proxy_auth {
    builder = builder.header(hyper::header::PROXY_AUTHORIZATION, auth);
  }
  let request = builder.body(Empty::<Bytes>::new()).unwrap();
  let response = sender.send_request(request).await.unwrap();
  let status = response.status();
  let body = response.into_body().collect().await.unwrap().to_bytes();
  (status, String::from_utf8_lossy(&body).into_owned())
}

/// Sends `CONNECT authority` and returns the status and the open stream.
async fn connect(socket: &Path, authority: &str) -> (u16, UnixStream) {
  connect_with_proxy_auth(socket, authority, None).await
}

async fn connect_with_proxy_auth(socket: &Path, authority: &str, proxy_auth: Option<&str>) -> (u16, UnixStream) {
  let mut stream = UnixStream::connect(socket).await.unwrap();
  let auth_line = proxy_auth.map(|a| format!("Proxy-Authorization: {a}\r\n")).unwrap_or_default();
  stream.write_all(format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n{auth_line}\r\n").as_bytes()).await.unwrap();
  let mut head = Vec::new();
  while !head.ends_with(b"\r\n\r\n") {
    let mut byte = [0u8; 1];
    assert_eq!(stream.read(&mut byte).await.unwrap(), 1, "the gateway closed during {:?}", String::from_utf8_lossy(&head));
    head.push(byte[0]);
  }
  let status = String::from_utf8_lossy(&head).split(' ').nth(1).unwrap().parse().unwrap();
  (status, stream)
}

// ROOT CAUSE:
//
// If an HTTP client tunnels plain HTTP through the proxy (Node's undici sends
// `CONNECT mock.needy:80` for `http://mock.needy`, as pi does), the gateway
// connected to the name `mock.needy` itself, which resolves nowhere, and
// answered 502: pi could not reach a service (seen 2026-11-13 with pi 1.5.2
// and the mock model as a service).
//
// Before the fix, a service was reached only by absolute-form requests.
// The fix keeps the tunnel opaque and points it at the service's URL.
#[tokio::test]
async fn a_connect_to_a_service_s_host_tunnels_to_its_url() {
  let (address, seen) = upstream().await;
  let gateway = start(address, "");

  let (status, mut stream) = connect(&gateway.socket, "model.needy:80").await;
  stream.write_all(b"GET /v1/models HTTP/1.1\r\nHost: model.needy\r\nConnection: close\r\n\r\n").await.unwrap();
  let mut reply = String::new();
  stream.read_to_string(&mut reply).await.unwrap();

  assert_eq!(status, 200);
  assert!(reply.starts_with("HTTP/1.1 200") && reply.ends_with("upstream"), "{reply}");
  assert_eq!(seen.lock().unwrap().iter().map(|s| s.path.as_str()).collect::<Vec<_>>(), ["/v1/models"]);
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
      proxy_authorization: None,
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
      via: None,
    }
  );
}

#[tokio::test]
async fn an_allowed_host_is_forwarded_to_itself() {
  let (address, seen) = upstream().await;
  let gateway = start_with("{ allow: [127.0.0.1] }", "", GatewayOptions::default().with_egress(EgressPolicy::allow_all()));

  let (status, _) = get(&gateway.socket, &format!("http://{address}/plain")).await;

  assert_eq!(status, StatusCode::OK);
  assert_eq!(
    *seen.lock().unwrap(),
    [Seen {
      path: "/plain".into(),
      authorization: None,
      proxy_authorization: None,
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

// ROOT CAUSE:
//
// If a `reset` fault fired for a `macos-vz` actor, its program waited until
// its own timeout (curl 28) instead of reading a reset, because the gateway
// only closed its side of the connection and vfkit's vsock relay passes no
// close to the guest, so the guest's forwarder never learned of it.
//
// Before the fix, the gateway closed the connection without a byte.
// The fix writes `GATEWAY_RESET` first, at a connection's start, so the
// forwarder resets the program's connection itself, whatever the relay.
#[tokio::test]
async fn a_reset_fault_tells_the_forwarder_and_closes_the_connection_without_an_answer() {
  let (address, _) = upstream().await;
  let gateway = start(address, "faults: { cut: {net: {connect: '*'}, reset: true} }");

  for request in [
    "GET http://model.needy/ HTTP/1.1\r\nHost: model.needy\r\n\r\n",
    "CONNECT 127.0.0.1:443 HTTP/1.1\r\nHost: 127.0.0.1:443\r\n\r\n",
  ] {
    let mut stream = UnixStream::connect(&gateway.socket).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut answer = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut answer)).await.expect("the gateway closes").unwrap();
    assert_eq!(answer, crate::contract::GATEWAY_RESET, "{request}: {:?}", String::from_utf8_lossy(&answer));
  }
}

#[tokio::test]
async fn a_reset_after_an_answer_on_the_same_connection_is_a_close_without_the_marker() {
  let (address, _) = upstream().await;
  // The first request to `model.needy` is answered, the second reset.
  let gateway = start(address, "faults: { cut: {net: {request: 'model.needy'}, reset: true, nth: 2} }");
  let mut stream = UnixStream::connect(&gateway.socket).await.unwrap();
  let request = "GET http://model.needy/ HTTP/1.1\r\nHost: model.needy\r\n\r\n";
  stream.write_all(format!("{request}{request}").as_bytes()).await.unwrap();

  let mut answer = Vec::new();
  tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut answer)).await.expect("the gateway closes").unwrap();

  let answer = String::from_utf8_lossy(&answer);
  assert!(answer.starts_with("HTTP/1.1 200"), "{answer:?}");
  assert!(!answer.contains("needy-reset"), "{answer:?}");
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
  let gateway = start_with("{ allow: [127.0.0.1] }", "", GatewayOptions::default().with_egress(EgressPolicy::allow_all()));

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
      via: None,
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
  let gateway = start_with("{ allow: [127.0.0.1] }", "", GatewayOptions::default().with_egress(EgressPolicy::allow_all()));
  let (status, mut tunnel) = connect(&gateway.socket, &address.to_string()).await;
  assert_eq!(status, 200);

  gateway.cancel.cancel();

  tokio::time::timeout(Duration::from_secs(5), gateway.task).await.expect("the gateway ends").unwrap();
  let mut rest = Vec::new();
  let read = tokio::time::timeout(Duration::from_secs(5), tunnel.read_to_end(&mut rest)).await.expect("the tunnel closes");
  // A closed tunnel reads end of file, or a reset.
  assert!(!matches!(read, Ok(n) if n > 0), "{rest:?}");
}

// ROOT CAUSE:
//
// If a client sent `CONNECT [::1]:443` or `CONNECT [2001:db8::1]:443`,
// `authority.split(':').next()` truncated the host at the first colon to `"["`,
// corrupting allowlist matching and trace events.
//
// Before the fix, IPv6 authorities were split on `':'`.
// The fix parses `http::uri::Authority` directly and strips brackets from the host.
#[test]
fn ipv6_connect_and_service_authorities_parse_without_truncation() {
  let loopback: hyper::http::uri::Authority = "[::1]:443".parse().unwrap();
  assert_eq!(connect::parse_connect_authority(Some(&loopback)), Some(("::1".into(), 443, "[::1]:443".into())));

  let doc: hyper::http::uri::Authority = "[2001:db8::1]:8443".parse().unwrap();
  assert_eq!(connect::parse_connect_authority(Some(&doc)), Some(("2001:db8::1".into(), 8443, "[2001:db8::1]:8443".into())));

  let no_port: hyper::http::uri::Authority = "[::1]".parse().unwrap();
  assert_eq!(connect::parse_connect_authority(Some(&no_port)), None);

  assert_eq!(connect::service_authority("http://[::1]:11434"), Some(("::1".into(), 11434)));
  assert_eq!(connect::service_authority("http://[2001:db8::1]"), Some(("2001:db8::1".into(), 80)));
  assert_eq!(connect::service_authority("https://[::1]:443"), None);
}

#[test]
fn egress_policy_blocks_loopback_link_local_ula_imds_unspecified_mapped_and_cidrs() {
  let default = EgressPolicy::default();

  // Loopback
  assert_eq!(default.check("127.0.0.1".parse::<IpAddr>().unwrap()), Err(BlockedReason::Loopback));
  assert_eq!(default.check("127.255.255.254".parse::<IpAddr>().unwrap()), Err(BlockedReason::Loopback));
  assert_eq!(default.check("::1".parse::<IpAddr>().unwrap()), Err(BlockedReason::Loopback));

  // Link-local & IPv4 cloud metadata
  assert_eq!(default.check("169.254.169.254".parse::<IpAddr>().unwrap()), Err(BlockedReason::LinkLocal));
  assert_eq!(default.check("169.254.0.1".parse::<IpAddr>().unwrap()), Err(BlockedReason::LinkLocal));
  assert_eq!(default.check("fe80::1".parse::<IpAddr>().unwrap()), Err(BlockedReason::LinkLocal));

  // IPv6 ULA & AWS IPv6 IMDS
  assert_eq!(default.check("fd00:ec2::254".parse::<IpAddr>().unwrap()), Err(BlockedReason::Ula));
  assert_eq!(default.check("fc00::1".parse::<IpAddr>().unwrap()), Err(BlockedReason::Ula));

  // Unspecified / 0.0.0.0/8
  assert_eq!(default.check("0.0.0.0".parse::<IpAddr>().unwrap()), Err(BlockedReason::Unspecified));
  assert_eq!(default.check("0.1.2.3".parse::<IpAddr>().unwrap()), Err(BlockedReason::Unspecified));
  assert_eq!(default.check("::".parse::<IpAddr>().unwrap()), Err(BlockedReason::Unspecified));

  // IPv4-mapped IPv6 targeting blocked IPv4 ranges
  assert_eq!(default.check("::ffff:127.0.0.1".parse::<IpAddr>().unwrap()), Err(BlockedReason::Loopback));
  assert_eq!(default.check("::ffff:169.254.169.254".parse::<IpAddr>().unwrap()), Err(BlockedReason::LinkLocal));
  assert_eq!(default.check("::ffff:0.0.0.0".parse::<IpAddr>().unwrap()), Err(BlockedReason::Unspecified));

  // Documentation / public IPs pass default policy
  assert_eq!(default.check("192.0.2.1".parse::<IpAddr>().unwrap()), Ok(()));
  assert_eq!(default.check("198.51.100.1".parse::<IpAddr>().unwrap()), Ok(()));
  assert_eq!(default.check("2001:db8::1".parse::<IpAddr>().unwrap()), Ok(()));

  // Operator-configured private CIDRs (e.g. RFC1918 + IPv6 range)
  let rfc1918_10: IpCidr = "10.0.0.0/8".parse().unwrap();
  let rfc1918_172: IpCidr = "172.16.0.0/12".parse().unwrap();
  let rfc1918_192: IpCidr = "192.168.0.0/16".parse().unwrap();
  let custom_v6: IpCidr = "2001:db8::/32".parse().unwrap();
  let strict = EgressPolicy::default().with_blocked_cidrs(vec![rfc1918_10, rfc1918_172, rfc1918_192, custom_v6]);

  assert_eq!(strict.check("10.1.2.3".parse::<IpAddr>().unwrap()), Err(BlockedReason::Cidr(rfc1918_10)));
  assert_eq!(strict.check("::ffff:10.1.2.3".parse::<IpAddr>().unwrap()), Err(BlockedReason::Cidr(rfc1918_10)));
  assert_eq!(strict.check("172.16.5.4".parse::<IpAddr>().unwrap()), Err(BlockedReason::Cidr(rfc1918_172)));
  assert_eq!(strict.check("192.168.1.1".parse::<IpAddr>().unwrap()), Err(BlockedReason::Cidr(rfc1918_192)));
  assert_eq!(strict.check("2001:db8::1".parse::<IpAddr>().unwrap()), Err(BlockedReason::Cidr(custom_v6)));
  assert_eq!(strict.check("198.51.100.1".parse::<IpAddr>().unwrap()), Ok(()));
}

#[tokio::test]
async fn ssrf_blocks_loopback_metadata_and_ipv6_even_with_wildcard_allowlist() {
  let (address, seen) = upstream().await;
  let gateway = start_with("{ allow: ['*'] }", "", GatewayOptions::default());

  // Direct HTTP to loopback is blocked
  let (status, body) = get(&gateway.socket, &format!("http://{address}/secret")).await;
  assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

  // Direct HTTP to localhost (DNS resolution -> loopback) is blocked
  let (status, body) = get(&gateway.socket, &format!("http://localhost:{}/secret", address.port())).await;
  assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

  // Direct HTTP to IPv4 IMDS and IPv4-mapped IPv6 IMDS is blocked
  let (status, body) = get(&gateway.socket, "http://169.254.169.254/latest/meta-data/").await;
  assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
  let (status, body) = get(&gateway.socket, "http://[::ffff:169.254.169.254]/latest/meta-data/").await;
  assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

  // CONNECT to loopback IPv4, IPv6, IPv4-mapped IPv6, and IPv6 IMDS are all blocked
  for authority in [
    address.to_string(),
    "[::1]:443".to_string(),
    "[::ffff:127.0.0.1]:443".to_string(),
    "[fd00:ec2::254]:80".to_string(),
  ] {
    let (status, _) = connect(&gateway.socket, &authority).await;
    assert_eq!(status, 403, "expected 403 for CONNECT {authority}");
  }

  assert!(seen.lock().unwrap().is_empty());

  // Verify the IPv6 CONNECT recorded unbracketed `::1`, never `"["`.
  let events = gateway.events().await;
  assert!(
    events.iter().any(|e| matches!(e, Event::NetDeny(d) if d.host == "::1" && d.path.is_none())),
    "expected NetDeny for ::1, got {events:?}"
  );
  assert!(!events.iter().any(|e| matches!(e, Event::NetDeny(d) if d.host == "[")), "truncated '[' host must never appear: {events:?}");
}

#[tokio::test]
async fn operator_configured_services_bypass_ssrf_blocking() {
  let (address, seen) = upstream().await;
  let strict = EgressPolicy::default().with_blocked_cidrs(vec![
    "10.0.0.0/8".parse().unwrap(),
    "192.168.0.0/16".parse().unwrap(),
  ]);
  let gateway =
    start_with(&format!("{{ services: {{ ollama: {{ url: \"http://{address}\" }} }} }}"), "", GatewayOptions::default().with_egress(strict));

  // Plain HTTP to configured service succeeds despite loopback target
  let (status, body) = get(&gateway.socket, "http://ollama.needy/api/tags").await;
  assert_eq!((status, body.as_str()), (StatusCode::OK, "upstream"));

  // CONNECT tunnel to configured service also succeeds despite loopback target
  let (status, mut stream) = connect(&gateway.socket, "ollama.needy:80").await;
  assert_eq!(status, 200);
  stream.write_all(b"GET /api/version HTTP/1.1\r\nHost: ollama.needy\r\nConnection: close\r\n\r\n").await.unwrap();
  let mut reply = String::new();
  stream.read_to_string(&mut reply).await.unwrap();
  assert!(reply.starts_with("HTTP/1.1 200") && reply.ends_with("upstream"), "{reply}");

  let paths: Vec<String> = seen.lock().unwrap().iter().map(|s| s.path.clone()).collect();
  assert_eq!(paths, ["/api/tags", "/api/version"]);
}

#[tokio::test]
async fn proxy_token_auth_enforces_bearer_token_and_strips_header() {
  let (address, seen) = upstream().await;
  let token = "needy-actor.run-123.0123456789abcdef";
  let gateway = start_with(
    &format!("{{ services: {{ model: {{ url: \"http://{address}\" }} }} }}"),
    "",
    GatewayOptions::default().with_auth_token(token),
  );

  // Missing Proxy-Authorization -> 407
  let (status, _) = get_with_proxy_auth(&gateway.socket, "http://model.needy/v1/models", None).await;
  assert_eq!(status, StatusCode::PROXY_AUTHENTICATION_REQUIRED);

  // Wrong token -> 407
  let (status, _) = get_with_proxy_auth(&gateway.socket, "http://model.needy/v1/models", Some("Bearer wrong-token")).await;
  assert_eq!(status, StatusCode::PROXY_AUTHENTICATION_REQUIRED);

  // Wrong scheme -> 407
  let (status, _) = get_with_proxy_auth(&gateway.socket, "http://model.needy/v1/models", Some(&format!("Basic {token}"))).await;
  assert_eq!(status, StatusCode::PROXY_AUTHENTICATION_REQUIRED);

  // CONNECT without token -> 407
  let (status, _) = connect_with_proxy_auth(&gateway.socket, "model.needy:80", None).await;
  assert_eq!(status, 407);

  // Upstream saw nothing from rejected requests
  assert!(seen.lock().unwrap().is_empty());

  // Valid token on HTTP GET -> 200 and Proxy-Authorization header stripped before upstream
  let bearer = format!("Bearer {token}");
  let (status, body) = get_with_proxy_auth(&gateway.socket, "http://model.needy/v1/models", Some(&bearer)).await;
  assert_eq!((status, body.as_str()), (StatusCode::OK, "upstream"));
  assert_eq!(
    *seen.lock().unwrap(),
    [Seen {
      path: "/v1/models".into(),
      authorization: None,
      proxy_authorization: None,
    }]
  );

  // Valid token on CONNECT -> 200 and tunnel works
  let (status, mut stream) = connect_with_proxy_auth(&gateway.socket, "model.needy:80", Some(&bearer)).await;
  assert_eq!(status, 200);
  stream.write_all(b"GET /tunneled HTTP/1.1\r\nHost: model.needy\r\nConnection: close\r\n\r\n").await.unwrap();
  let mut reply = String::new();
  stream.read_to_string(&mut reply).await.unwrap();
  assert!(reply.starts_with("HTTP/1.1 200") && reply.ends_with("upstream"), "{reply}");
}

#[tokio::test]
async fn on_activity_fires_for_authenticated_http_and_connect_requests_only() {
  let (address, _) = upstream().await;
  let hits = Arc::new(AtomicUsize::new(0));
  let counter = hits.clone();
  let token = "needy-actor.run-456.fedcba9876543210";
  let gateway = start_with(
    &format!("{{ services: {{ model: {{ url: \"http://{address}\" }} }} }}"),
    "",
    GatewayOptions::default().with_auth_token(token).with_on_activity(Arc::new(move || {
      counter.fetch_add(1, Ordering::SeqCst);
    })),
  );

  // Unauthenticated request must NOT trigger activity
  let (status, _) = get_with_proxy_auth(&gateway.socket, "http://model.needy/v1/models", None).await;
  assert_eq!(status, StatusCode::PROXY_AUTHENTICATION_REQUIRED);
  assert_eq!(hits.load(Ordering::SeqCst), 0);

  let bearer = format!("Bearer {token}");

  // Authenticated HTTP request triggers activity
  let (status, _) = get_with_proxy_auth(&gateway.socket, "http://model.needy/v1/models", Some(&bearer)).await;
  assert_eq!(status, StatusCode::OK);
  assert_eq!(hits.load(Ordering::SeqCst), 1);

  // Authenticated CONNECT request triggers activity
  let (status, _) = connect_with_proxy_auth(&gateway.socket, "model.needy:80", Some(&bearer)).await;
  assert_eq!(status, 200);
  assert_eq!(hits.load(Ordering::SeqCst), 2);
}

/// The upstream `scheme://` the test proxy at `proxy`, with `credentials`.
fn through(proxy: SocketAddr, scheme: &str, credentials: Option<std::path::PathBuf>) -> Upstream {
  Upstream::socks5(&format!("{scheme}://{proxy}"), credentials).unwrap()
}

/// Writes `USERNAME:PASSWORD` to `path`, private to this user.
fn write_credentials(path: &Path, text: &str) {
  use std::os::unix::fs::PermissionsExt as _;
  std::fs::write(path, text).unwrap();
  std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

/// What the tunnel's upstream answers to one request sent through it.
async fn tunneled(mut tunnel: UnixStream, path: &str) -> String {
  tunnel.write_all(format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
  let mut reply = String::new();
  tunnel.read_to_string(&mut reply).await.unwrap();
  reply
}

// `*.invalid` resolves nowhere (RFC 6761 section 6.4), so a request that
// reaches the upstream proves the gateway did not resolve the name itself.
#[tokio::test]
async fn through_a_socks5h_upstream_requests_and_tunnels_send_the_name_unresolved() {
  let (address, seen) = upstream().await;
  let proxy = Socks5Server::start(address).await;
  let via = format!("socks5h://{}", proxy.address());
  let gateway =
    start_with("{ allow: [api.invalid] }", "", GatewayOptions::default().with_upstream(through(proxy.address(), "socks5h", None)));

  let (status, body) = get(&gateway.socket, "http://api.invalid/plain").await;
  let (tunnel_status, tunnel) = connect(&gateway.socket, "api.invalid:443").await;
  let reply = tunneled(tunnel, "/tunneled").await;

  assert_eq!((status, body.as_str()), (StatusCode::OK, "upstream"));
  assert_eq!(tunnel_status, 200);
  assert!(reply.starts_with("HTTP/1.1 200") && reply.ends_with("upstream"), "{reply}");
  let names = |target: &str| ProxySeen {
    target: target.into(),
    name: true,
    credentials: None,
  };
  assert_eq!(proxy.seen(), [names("api.invalid:80"), names("api.invalid:443")]);
  assert_eq!(seen.lock().unwrap().iter().map(|s| s.path.as_str()).collect::<Vec<_>>(), ["/plain", "/tunneled"]);
  let events = gateway.events().await;
  assert!(
    events.iter().any(|e| matches!(e, Event::NetRequest(r) if r.host == "api.invalid" && r.via.as_deref() == Some(via.as_str()))),
    "{events:?}"
  );
  assert!(
    events.iter().any(|e| matches!(e, Event::NetConnect(c) if c.authority == "api.invalid:443" && c.via.as_deref() == Some(via.as_str()))),
    "{events:?}"
  );
}

#[tokio::test]
async fn through_a_socks5_upstream_the_gateway_resolves_validates_and_sends_the_address() {
  let (address, _) = upstream().await;
  let proxy = Socks5Server::start(address).await;
  let port = address.port();

  // `localhost` resolves to loopback here, which the default policy blocks
  // before the proxy is asked.
  let strict = start_with("{ allow: [localhost] }", "", GatewayOptions::default().with_upstream(through(proxy.address(), "socks5", None)));
  let (status, body) = get(&strict.socket, &format!("http://localhost:{port}/")).await;
  assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
  assert!(proxy.seen().is_empty(), "{:?}", proxy.seen());

  let open = start_with(
    "{ allow: [localhost] }",
    "",
    GatewayOptions::default().with_egress(EgressPolicy::allow_all()).with_upstream(through(proxy.address(), "socks5", None)),
  );
  let (status, _) = get(&open.socket, &format!("http://localhost:{port}/")).await;
  assert_eq!(status, StatusCode::OK);
  let [sent] = &proxy.seen()[..] else {
    panic!("{:?}", proxy.seen());
  };
  assert!(!sent.name, "{sent:?}");
  assert!(sent.target.ends_with(&format!(":{port}")) && !sent.target.starts_with("localhost"), "{sent:?}");
}

#[tokio::test]
async fn an_ip_literal_is_still_checked_against_the_egress_policy_through_socks5h() {
  let (address, seen) = upstream().await;
  let proxy = Socks5Server::start(address).await;
  let gateway = start_with("{ allow: [127.0.0.1] }", "", GatewayOptions::default().with_upstream(through(proxy.address(), "socks5h", None)));

  let (status, _) = connect(&gateway.socket, &address.to_string()).await;
  let (http, _) = get(&gateway.socket, &format!("http://{address}/")).await;

  assert_eq!((status, http), (403, StatusCode::FORBIDDEN));
  assert!(proxy.seen().is_empty());
  assert!(seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_service_is_dialed_directly_not_through_the_upstream() {
  let (address, seen) = upstream().await;
  let proxy = Socks5Server::start(address).await;
  let gateway = start_with(
    &format!("{{ services: {{ model: {{ url: \"http://{address}\" }} }} }}"),
    "",
    GatewayOptions::default().with_upstream(through(proxy.address(), "socks5h", None)),
  );

  let (status, _) = get(&gateway.socket, "http://model.needy/v1/models").await;
  let (tunnel_status, tunnel) = connect(&gateway.socket, "model.needy:80").await;
  let reply = tunneled(tunnel, "/v1/version").await;

  assert_eq!((status, tunnel_status), (StatusCode::OK, 200));
  assert!(reply.ends_with("upstream"), "{reply}");
  assert!(proxy.seen().is_empty(), "{:?}", proxy.seen());
  assert_eq!(seen.lock().unwrap().len(), 2);
  let events = gateway.events().await;
  assert!(
    events.iter().all(|e| match e {
      Event::NetRequest(r) => r.via.is_none(),
      Event::NetConnect(c) => c.via.is_none(),
      _ => true,
    }),
    "{events:?}"
  );
}

#[tokio::test]
async fn an_unreachable_upstream_fails_closed_with_502_and_never_connects_directly() {
  let (address, seen) = upstream().await;
  // A port nothing listens on any more.
  let closed = TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap();
  let gateway = start_with(
    "{ allow: [127.0.0.1] }",
    "",
    GatewayOptions::default().with_egress(EgressPolicy::allow_all()).with_upstream(through(closed, "socks5h", None)),
  );

  let (status, body) = get(&gateway.socket, &format!("http://{address}/direct")).await;
  let (tunnel, _) = connect(&gateway.socket, &address.to_string()).await;

  assert_eq!((status, tunnel), (StatusCode::BAD_GATEWAY, 502));
  assert!(body.contains(&format!("upstream proxy socks5h://{closed} is unreachable")), "{body}");
  assert!(seen.lock().unwrap().is_empty(), "the gateway fell back to a direct connection");
  let events = gateway.events().await;
  let errors: Vec<&NetError> = events
    .iter()
    .filter_map(|e| match e {
      Event::NetError(error) => Some(error),
      _ => None,
    })
    .collect();
  assert_eq!(errors.len(), 2, "{events:?}");
  for error in errors {
    assert!(error.error.contains("is unreachable"), "{error:?}");
    assert_eq!(error.via, Some(format!("socks5h://{closed}")));
  }
}

#[tokio::test]
async fn an_upstream_that_refuses_the_target_answers_502_with_its_reason() {
  let closed = TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap();
  // The proxy itself answers, but cannot reach the target.
  let proxy = Socks5Server::start(closed).await;
  let gateway =
    start_with("{ allow: [api.invalid] }", "", GatewayOptions::default().with_upstream(through(proxy.address(), "socks5h", None)));

  let (status, _) = connect(&gateway.socket, "api.invalid:443").await;

  assert_eq!(status, 502);
  let events = gateway.events().await;
  assert!(
    events.iter().any(|e| matches!(e, Event::NetError(n) if n.host == "api.invalid" && n.error.contains("Connection refused"))),
    "{events:?}"
  );
}

#[tokio::test]
async fn the_upstream_authenticates_with_the_credentials_file_read_again_for_each_connection() {
  let (address, _) = upstream().await;
  let proxy = Socks5Server::start(address).await;
  let dir = tempfile::tempdir().unwrap();
  let file = dir.path().join("credentials");
  write_credentials(&file, "alice:first-s3cret\n");
  proxy.require(Some(("alice", "first-s3cret")));
  let gateway = start_with(
    "{ allow: ['*.invalid'] }",
    "",
    GatewayOptions::default().with_upstream(through(proxy.address(), "socks5h", Some(file.clone()))),
  );

  let (first, _) = get(&gateway.socket, "http://a.invalid/").await;
  // The Secret rotates on both sides; the next connection uses the new one.
  write_credentials(&file, "bob:second-s3cret\n");
  proxy.require(Some(("bob", "second-s3cret")));
  let (second, _) = connect(&gateway.socket, "b.invalid:443").await;
  // The proxy rotates, the file does not (yet): the proxy refuses.
  proxy.require(Some(("carol", "third-s3cret")));
  let (refused, refusal) = get(&gateway.socket, "http://c.invalid/").await;

  assert_eq!((first, second, refused), (StatusCode::OK, 200, StatusCode::BAD_GATEWAY));
  let credentials: Vec<_> = proxy.seen().into_iter().map(|seen| seen.credentials).collect();
  assert_eq!(
    credentials,
    [
      Some(("alice".to_string(), "first-s3cret".to_string())),
      Some(("bob".to_string(), "second-s3cret".to_string()))
    ]
  );
  assert!(refusal.contains("Password auth failure"), "{refusal}");
  // Neither the answers nor the trace carry a password.
  let events = gateway.events().await;
  let recorded: String = events.iter().map(Event::payload).collect();
  assert!(recorded.contains("Password auth failure"), "{recorded}");
  for text in [
    &recorded,
    &refusal,
    &format!("{:?}", through(proxy.address(), "socks5h", Some(file))),
  ] {
    assert!(!text.contains("s3cret"), "a password leaked: {text}");
  }
}

#[tokio::test]
async fn a_credentials_file_others_can_read_fails_the_connection_before_the_proxy_is_asked() {
  use std::os::unix::fs::PermissionsExt as _;
  let (address, _) = upstream().await;
  let proxy = Socks5Server::start(address).await;
  let dir = tempfile::tempdir().unwrap();
  let file = dir.path().join("credentials");
  write_credentials(&file, "alice:s3cret\n");
  std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
  let gateway =
    start_with("{ allow: [api.invalid] }", "", GatewayOptions::default().with_upstream(through(proxy.address(), "socks5h", Some(file))));

  let (status, body) = get(&gateway.socket, "http://api.invalid/").await;

  assert_eq!(status, StatusCode::BAD_GATEWAY);
  assert!(body.contains("other users can access it"), "{body}");
  assert!(proxy.seen().is_empty());
}
