use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UnixStream};

use super::*;

/// A program's connection to the forwarder, the gateway's end of the
/// forwarder's connection, and the forwarder relaying between them.
async fn relayed() -> (TcpStream, UnixStream, tokio::task::JoinHandle<std::io::Result<()>>) {
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let program = TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
  let (inbound, _) = listener.accept().await.unwrap();
  let (outbound, gateway) = UnixStream::pair().unwrap();
  let relay = tokio::spawn(relay(inbound, outbound));
  (program, gateway, relay)
}

// ROOT CAUSE:
//
// If a `reset` fault fired for a `macos-vz` actor, its program waited until
// its own timeout instead of reading a reset, because the forwarder only
// passed on a close of the gateway's connection, which vfkit's vsock relay
// never delivers to the guest.
//
// Before the fix, the forwarder copied the gateway's bytes as they came.
// The fix resets the program's connection when the gateway's first bytes
// are `GATEWAY_RESET`, so the program reads a reset on every driver.
#[tokio::test]
async fn a_gateway_that_starts_with_the_reset_marker_resets_the_programs_connection() {
  let (mut program, mut gateway, relay) = relayed().await;
  program.write_all(b"CONNECT example.com:443 HTTP/1.1\r\n\r\n").await.unwrap();
  let mut request = [0u8; 8];
  gateway.read_exact(&mut request).await.unwrap();

  // In two writes, as a relay may split it; the gateway's side stays open,
  // as vfkit keeps it.
  gateway.write_all(&GATEWAY_RESET[..3]).await.unwrap();
  tokio::time::sleep(Duration::from_millis(50)).await;
  gateway.write_all(&GATEWAY_RESET[3..]).await.unwrap();

  let mut answer = Vec::new();
  let read =
    tokio::time::timeout(Duration::from_secs(5), program.read_to_end(&mut answer)).await.expect("the program's connection stays open");
  assert_eq!(read.map_err(|e| e.kind()), Err(std::io::ErrorKind::ConnectionReset), "{answer:?}");
  tokio::time::timeout(Duration::from_secs(5), relay).await.unwrap().unwrap().unwrap();
}

#[tokio::test]
async fn any_other_answer_passes_both_ways_until_both_close() {
  let (mut program, mut gateway, relay) = relayed().await;
  program.write_all(b"GET http://svc.needy/ HTTP/1.1\r\n\r\n").await.unwrap();
  program.shutdown().await.unwrap();
  let mut request = Vec::new();
  gateway.read_to_end(&mut request).await.unwrap();
  assert_eq!(request, b"GET http://svc.needy/ HTTP/1.1\r\n\r\n");

  // A first byte the marker also starts with, then something else.
  gateway.write_all(b"\0no").await.unwrap();
  gateway.write_all(b"t a reset").await.unwrap();
  drop(gateway);

  let mut answer = Vec::new();
  program.read_to_end(&mut answer).await.unwrap();
  assert_eq!(answer, b"\0not a reset");
  tokio::time::timeout(Duration::from_secs(5), relay).await.unwrap().unwrap().unwrap();
}

#[tokio::test]
async fn a_gateway_that_closes_at_once_closes_the_programs_connection() {
  let (mut program, gateway, relay) = relayed().await;
  drop(gateway);

  let mut answer = Vec::new();
  program.read_to_end(&mut answer).await.unwrap();
  assert!(answer.is_empty());
  program.shutdown().await.unwrap();
  tokio::time::timeout(Duration::from_secs(5), relay).await.unwrap().unwrap().unwrap();
}
