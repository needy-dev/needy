use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

use super::*;

/// A loopback echo server; returns its port.
fn echo() -> u16 {
  let listener = TcpListener::bind("127.0.0.1:0").unwrap();
  let port = listener.local_addr().unwrap().port();
  std::thread::spawn(move || {
    for stream in listener.incoming() {
      let Ok(mut stream) = stream else { return };
      std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(n) = stream.read(&mut buf) {
          if n == 0 || stream.write_all(&buf[..n]).is_err() {
            return;
          }
        }
      });
    }
  });
  port
}

fn round_trip(stream: &mut TcpStream) -> std::io::Result<Duration> {
  let start = Instant::now();
  stream.write_all(b"ping")?;
  let mut buf = [0u8; 4];
  stream.read_exact(&mut buf)?;
  assert_eq!(&buf, b"ping");
  Ok(start.elapsed())
}

fn client(port: u16) -> TcpStream {
  let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
  stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
  stream
}

#[test]
fn an_idle_connection_is_dropped_and_a_busy_one_kept() {
  let proxy = LinkProxy::start(echo(), Some(Duration::from_millis(400)), Duration::ZERO).unwrap();
  let mut idle = client(proxy.port);
  let mut busy = client(proxy.port);
  round_trip(&mut idle).unwrap();

  for _ in 0..8 {
    round_trip(&mut busy).unwrap();
    std::thread::sleep(Duration::from_millis(150));
  }

  assert!(round_trip(&mut idle).is_err(), "a connection idle for 1.2 s survived a 400 ms idle limit");
  round_trip(&mut busy).unwrap();
}

#[test]
fn a_cut_drops_every_open_connection_and_new_ones_work() {
  let proxy = LinkProxy::start(echo(), None, Duration::ZERO).unwrap();
  let mut first = client(proxy.port);
  round_trip(&mut first).unwrap();

  proxy.cut();

  assert!(round_trip(&mut first).is_err());
  round_trip(&mut client(proxy.port)).unwrap();
}

#[test]
fn a_delay_adds_twice_itself_to_a_round_trip() {
  let proxy = LinkProxy::start(echo(), None, Duration::from_millis(100)).unwrap();
  let mut stream = client(proxy.port);

  let rtt = round_trip(&mut stream).unwrap();

  assert!(rtt >= Duration::from_millis(200), "{rtt:?}");
  assert!(rtt < Duration::from_secs(2), "{rtt:?}");
}

#[test]
fn transports_without_settings_are_not_covered_with_the_reason() {
  let settings = Settings::default();

  assert_eq!(settings.blocker(Transport::Direct), None);
  assert_eq!(settings.blocker(Transport::IdleProxy), None);
  assert!(settings.blocker(Transport::SshL).unwrap().contains("ssh.destination"));
  assert!(settings.blocker(Transport::Cloudflare).unwrap().contains("cloudflare"));
}

#[test]
fn cloudflare_settings_name_the_variables_that_are_missing() {
  let settings: Settings = serde_json::from_str(
    r#"{ "cloudflare": { "hostname": "needy.example.com", "origin_port": 7400,
       "tunnel_token_env": "NEEDY_E2E_TEST_UNSET_TUNNEL", "client_id_env": "NEEDY_E2E_TEST_UNSET_ID",
       "client_secret_env": "NEEDY_E2E_TEST_UNSET_SECRET" } }"#,
  )
  .unwrap();

  let reason = settings.blocker(Transport::Cloudflare).unwrap();

  assert!(reason.contains("NEEDY_E2E_TEST_UNSET_TUNNEL"), "{reason}");
}
