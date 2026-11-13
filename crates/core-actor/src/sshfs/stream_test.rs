use std::sync::atomic::{AtomicBool, Ordering};

use needy_api_proto::v1::{
  ExecEnded, ExecExited, ExecRequest, ExecResponse, ExecStarted, KillProcessRequest, ListProcessesRequest, ListProcessesResponse,
  exec_request, exec_response,
};
use needy_common::sftp::{SSH_FX_CONNECTION_LOST, SSH_FXP_STATUS, read_u32};
use tokio::io::DuplexStream;
use tokio_stream::wrappers::ReceiverStream;
use tonic::Status;

use super::*;

// Source: https://github.com/openssh/openssh-portable/blob/V_10_6_P2/sftp.h#L46
// (`SSH2_FXP_STAT 17`).
const SSH_FXP_STAT: u8 = 17;

/// An SFTP packet of `kind` with `body` after the type.
fn packet(kind: u8, body: &[u8]) -> Vec<u8> {
  let mut out = ((body.len() + 1) as u32).to_be_bytes().to_vec();
  out.push(kind);
  out.extend(body);
  out
}

fn init() -> Vec<u8> {
  packet(SSH_FXP_INIT, &3u32.to_be_bytes())
}

fn stat(id: u32) -> Vec<u8> {
  let mut body = id.to_be_bytes().to_vec();
  body.extend(1u32.to_be_bytes());
  body.push(b'.');
  packet(SSH_FXP_STAT, &body)
}

/// One `sftp-server` the fake started: what it received, and its side of
/// the stream, to end it as a lost connection or an exit would.
struct Server {
  received: Arc<Mutex<Vec<Vec<u8>>>>,
  responses: mpsc::Sender<Result<ExecResponse, Status>>,
}

/// An executor whose every exec is a stand-in `sftp-server`: it answers
/// INIT with VERSION and any request with an OK status, unless holding.
#[derive(Default)]
struct Fake {
  servers: Mutex<Vec<Server>>,
  /// Requests get no reply while set, so they stay in flight.
  hold: Arc<AtomicBool>,
  /// New execs fail while set, as with a runner that is away.
  refuse: AtomicBool,
}

#[async_trait::async_trait]
impl Executor for Fake {
  async fn exec(&self, mut requests: mpsc::Receiver<ExecRequest>) -> Result<needy_api_client::Responses, Status> {
    if self.refuse.load(Ordering::SeqCst) {
      return Err(Status::unavailable("runner r is not connected"));
    }
    let (tx, rx) = mpsc::channel(64);
    let received = Arc::new(Mutex::new(Vec::new()));
    self.servers.lock().unwrap().push(Server {
      received: received.clone(),
      responses: tx.clone(),
    });
    let hold = self.hold.clone();
    tokio::spawn(async move {
      let mut framed = Vec::new();
      while let Some(request) = requests.recv().await {
        let bytes = match request.payload {
          Some(exec_request::Payload::Start(_)) => {
            let started = ExecResponse {
              diagnostic: None,
              payload: Some(exec_response::Payload::Started(ExecStarted::default())),
            };
            let _sent = tx.send(Ok(started)).await;
            continue;
          }
          Some(exec_request::Payload::Stdin(bytes)) => bytes,
          _ => continue,
        };
        framed.extend(bytes);
        for p in take_packets(&mut framed) {
          received.lock().unwrap().push(p.clone());
          let reply = match (packet_type(&p), request_id(&p)) {
            (Some(SSH_FXP_INIT), _) => packet(SSH_FXP_VERSION, &3u32.to_be_bytes()),
            (_, Some(id)) if !hold.load(Ordering::SeqCst) => status_reply(id, 0, "ok"),
            _ => continue,
          };
          let stdout = ExecResponse {
            diagnostic: None,
            payload: Some(exec_response::Payload::Stdout(reply)),
          };
          // The bridge may have left this server; nothing to do then.
          let _sent = tx.send(Ok(stdout)).await;
        }
      }
    });
    Ok(Box::pin(ReceiverStream::new(rx)))
  }

  async fn list_processes(&self, _: ListProcessesRequest) -> Result<ListProcessesResponse, Status> {
    unimplemented!()
  }

  async fn kill_process(&self, _: KillProcessRequest) -> Result<(), Status> {
    unimplemented!()
  }
}

impl Fake {
  fn servers(&self) -> usize {
    self.servers.lock().unwrap().len()
  }

  fn received(&self, server: usize) -> Vec<Vec<u8>> {
    self.servers.lock().unwrap()[server].received.lock().unwrap().clone()
  }

  /// Ends server `server`'s stream as the control plane does when the
  /// runner's connection drops.
  async fn lose(&self, server: usize) {
    let tx = self.servers.lock().unwrap()[server].responses.clone();
    let ended = ExecResponse {
      diagnostic: None,
      payload: Some(exec_response::Payload::Ended(ExecEnded {
        reason: "runner r disconnected".into(),
      })),
    };
    tx.send(Ok(ended)).await.unwrap();
  }

  async fn exit(&self, server: usize) {
    let tx = self.servers.lock().unwrap()[server].responses.clone();
    let exited = ExecResponse {
      diagnostic: None,
      payload: Some(exec_response::Payload::Exited(ExecExited {
        signal: "SIGKILL".into(),
        ..Default::default()
      })),
    };
    tx.send(Ok(exited)).await.unwrap();
  }
}

/// sshfs's side of a bridge: what it writes, and the packets it reads.
struct Sshfs {
  writes: DuplexStream,
  reads: DuplexStream,
  framed: Vec<u8>,
}

impl Sshfs {
  async fn send(&mut self, bytes: &[u8]) {
    self.writes.write_all(bytes).await.unwrap();
  }

  /// The next packet sshfs reads; `None` once the bridge closed its input.
  async fn next(&mut self) -> Option<Vec<u8>> {
    loop {
      if let Some(p) = take_packets(&mut self.framed).into_iter().next() {
        return Some(p);
      }
      let mut buf = [0u8; 4096];
      let n = tokio::time::timeout(Duration::from_secs(5), self.reads.read(&mut buf)).await.expect("sshfs got nothing within 5 s").unwrap();
      if n == 0 {
        return None;
      }
      self.framed.extend(&buf[..n]);
    }
  }
}

async fn start(fake: Arc<Fake>, window: Duration) -> (Sshfs, tokio::task::JoinHandle<()>) {
  let (writes, from_sshfs) = tokio::io::duplex(1 << 16);
  let (to_sshfs, reads) = tokio::io::duplex(1 << 16);
  let start = ExecStart {
    argv: vec!["sftp-server".into()],
    cwd: "/w".into(),
    ..Default::default()
  };
  let executor: Arc<dyn Executor> = fake.clone();
  let first = ExecStream::start(executor.as_ref(), start.clone()).await.unwrap();
  let bridge = tokio::spawn(bridge(executor, start, first, from_sshfs, to_sshfs, "/t".into(), window));
  (
    Sshfs {
      writes,
      reads,
      framed: Vec::new(),
    },
    bridge,
  )
}

/// Waits until server `server` received `count` packets.
async fn wait_received(fake: &Fake, server: usize, count: usize) {
  let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
  while fake.servers() <= server || fake.received(server).len() < count {
    assert!(tokio::time::Instant::now() < deadline, "server {server} did not receive {count} packets");
    tokio::time::sleep(Duration::from_millis(10)).await;
  }
}

fn status_of(p: &[u8]) -> (u32, u32) {
  assert_eq!(packet_type(p), Some(SSH_FXP_STATUS), "{p:?}");
  (read_u32(&p[5..]).unwrap(), read_u32(&p[9..]).unwrap())
}

// ROOT CAUSE:
//
// If a workspace mount's exec stream was lost (a runner's tunnel dropped, as
// Cloudflare's edge does when it restarts), the mount died for good: sshfs in
// passive mode cannot reconnect, and its requests in flight never got an
// answer.
//
// Before the fix, the bridge stopped at the first lost stream.
// The fix keeps the mount: requests in flight fail as lost, a new
// sftp-server gets sshfs's INIT replayed, its VERSION stays from sshfs,
// and later requests reach it.
#[tokio::test]
async fn a_lost_stream_fails_the_requests_in_flight_and_the_next_ones_reach_a_new_server() {
  let fake = Arc::new(Fake::default());
  let (mut sshfs, _bridge) = start(fake.clone(), Duration::from_secs(10)).await;
  sshfs.send(&init()).await;
  assert_eq!(packet_type(&sshfs.next().await.unwrap()), Some(SSH_FXP_VERSION));
  fake.hold.store(true, Ordering::SeqCst);
  sshfs.send(&stat(7)).await;
  wait_received(&fake, 0, 2).await;

  fake.lose(0).await;

  assert_eq!(status_of(&sshfs.next().await.unwrap()), (7, SSH_FX_CONNECTION_LOST));
  fake.hold.store(false, Ordering::SeqCst);
  sshfs.send(&stat(8)).await;
  // The VERSION of the new server never reaches sshfs: the next packet is
  // the answer to 8.
  assert_eq!(status_of(&sshfs.next().await.unwrap()), (8, 0));
  assert_eq!(fake.servers(), 2);
  let second = fake.received(1);
  assert_eq!(packet_type(&second[0]), Some(SSH_FXP_INIT), "INIT is replayed first");
  assert_eq!(request_id(&second[1]), Some(8));
}

#[tokio::test]
async fn requests_sent_while_the_stream_is_away_wait_for_the_new_server() {
  let fake = Arc::new(Fake::default());
  let (mut sshfs, _bridge) = start(fake.clone(), Duration::from_secs(10)).await;
  sshfs.send(&init()).await;
  sshfs.next().await.unwrap();
  fake.refuse.store(true, Ordering::SeqCst);

  fake.lose(0).await;
  sshfs.send(&stat(9)).await;
  tokio::time::sleep(Duration::from_millis(600)).await;
  fake.refuse.store(false, Ordering::SeqCst);

  assert_eq!(status_of(&sshfs.next().await.unwrap()), (9, 0));
  assert_eq!(fake.servers(), 2);
}

#[tokio::test]
async fn a_server_that_exits_ends_the_mount_without_starting_another() {
  let fake = Arc::new(Fake::default());
  let (mut sshfs, bridge) = start(fake.clone(), Duration::from_secs(10)).await;
  sshfs.send(&init()).await;
  sshfs.next().await.unwrap();
  fake.hold.store(true, Ordering::SeqCst);
  sshfs.send(&stat(3)).await;
  wait_received(&fake, 0, 2).await;

  fake.exit(0).await;

  assert_eq!(status_of(&sshfs.next().await.unwrap()), (3, SSH_FX_CONNECTION_LOST));
  assert_eq!(sshfs.next().await, None, "sshfs's input ends with the mount");
  tokio::time::timeout(Duration::from_secs(5), bridge).await.unwrap().unwrap();
  assert_eq!(fake.servers(), 1);
}

#[tokio::test]
async fn a_stream_that_stays_away_past_the_window_ends_the_mount() {
  let fake = Arc::new(Fake::default());
  let (mut sshfs, bridge) = start(fake.clone(), Duration::from_millis(500)).await;
  sshfs.send(&init()).await;
  sshfs.next().await.unwrap();
  fake.refuse.store(true, Ordering::SeqCst);

  fake.lose(0).await;

  assert_eq!(sshfs.next().await, None, "sshfs's input ends with the mount");
  tokio::time::timeout(Duration::from_secs(5), bridge).await.unwrap().unwrap();
  assert_eq!(fake.servers(), 1);
}
