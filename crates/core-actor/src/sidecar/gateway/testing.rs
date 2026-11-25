//! A SOCKS5 proxy for tests of the gateway's upstream: it records what each
//! client asked for and connects every CONNECT to one address, whatever it
//! names, so a test can use names that resolve nowhere (`*.invalid`) and
//! still reach its server.
//!
//! Hand-written because `tokio-socks` has no server side and the proxy
//! servers on crates.io resolve and connect for real; this one must not. It
//! speaks only what the gateway uses: no authentication or
//! username/password, and CONNECT.
// Source: RFC 1928 sections 3 to 6 (method selection, request, replies),
// https://www.rfc-editor.org/rfc/rfc1928
// Source: RFC 1929 section 2 (username/password subnegotiation, version 1),
// https://www.rfc-editor.org/rfc/rfc1929#section-2

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};

/// What one client sent the proxy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
  /// The CONNECT target as sent: `NAME:PORT` for a domain name (address
  /// type 3), `IP:PORT` for an address.
  pub target: String,
  /// Whether the target was a domain name the proxy was to resolve.
  pub name: bool,
  /// The username and password the client authenticated with, if it did.
  pub credentials: Option<(String, String)>,
}

/// A running test proxy. It stops with the test's runtime.
pub struct Socks5Server {
  address: SocketAddr,
  seen: Arc<Mutex<Vec<Seen>>>,
  required: Arc<Mutex<Option<(String, String)>>>,
}

impl Socks5Server {
  /// A proxy on loopback that connects every CONNECT to `forward_to`.
  pub async fn start(forward_to: SocketAddr) -> Self {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind the test SOCKS5 proxy");
    let address = listener.local_addr().expect("the test SOCKS5 proxy's address");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let required = Arc::new(Mutex::new(None));
    let (record, require) = (seen.clone(), required.clone());
    tokio::spawn(async move {
      while let Ok((stream, _)) = listener.accept().await {
        let (record, require) = (record.clone(), require.clone());
        tokio::spawn(async move {
          // A client that goes away mid-handshake is no failure of the test.
          let _ = serve(stream, forward_to, record, require).await;
        });
      }
    });
    Self {
      address,
      seen,
      required,
    }
  }

  /// Requires username/password authentication with these credentials
  /// from now on, or none.
  pub fn require(&self, credentials: Option<(&str, &str)>) {
    *self.required.lock().unwrap() = credentials.map(|(u, p)| (u.to_string(), p.to_string()));
  }

  pub fn address(&self) -> SocketAddr {
    self.address
  }

  /// What clients sent, in order of their requests.
  pub fn seen(&self) -> Vec<Seen> {
    self.seen.lock().unwrap().clone()
  }
}

async fn serve(
  mut client: TcpStream,
  forward_to: SocketAddr,
  seen: Arc<Mutex<Vec<Seen>>>,
  required: Arc<Mutex<Option<(String, String)>>>,
) -> std::io::Result<()> {
  // Method selection: VER, NMETHODS, METHODS (RFC 1928 section 3).
  let mut head = [0u8; 2];
  client.read_exact(&mut head).await?;
  let mut methods = vec![0u8; usize::from(head[1])];
  client.read_exact(&mut methods).await?;
  let required = required.lock().unwrap().clone();
  let mut credentials = None;
  match &required {
    None => client.write_all(&[5, 0x00]).await?,
    Some(_) if !methods.contains(&0x02) => {
      // X'FF': no acceptable methods.
      return client.write_all(&[5, 0xff]).await;
    }
    Some((username, password)) => {
      client.write_all(&[5, 0x02]).await?;
      // VER 1, ULEN, UNAME, PLEN, PASSWD (RFC 1929 section 2).
      let mut version = [0u8; 2];
      client.read_exact(&mut version).await?;
      let mut user = vec![0u8; usize::from(version[1])];
      client.read_exact(&mut user).await?;
      let mut length = [0u8; 1];
      client.read_exact(&mut length).await?;
      let mut pass = vec![0u8; usize::from(length[0])];
      client.read_exact(&mut pass).await?;
      let sent = (String::from_utf8_lossy(&user).into_owned(), String::from_utf8_lossy(&pass).into_owned());
      if (&sent.0, &sent.1) != (username, password) {
        // A STATUS other than X'00' is a failure, and the server closes.
        return client.write_all(&[1, 0x01]).await;
      }
      client.write_all(&[1, 0x00]).await?;
      credentials = Some(sent);
    }
  }
  // Request: VER, CMD, RSV, ATYP, DST.ADDR, DST.PORT (RFC 1928 section 4).
  let mut request = [0u8; 4];
  client.read_exact(&mut request).await?;
  let (target, name) = match request[3] {
    0x01 => {
      let mut ip = [0u8; 4];
      client.read_exact(&mut ip).await?;
      (IpAddr::V4(Ipv4Addr::from(ip)).to_string(), false)
    }
    0x04 => {
      let mut ip = [0u8; 16];
      client.read_exact(&mut ip).await?;
      (format!("[{}]", Ipv6Addr::from(ip)), false)
    }
    0x03 => {
      let mut length = [0u8; 1];
      client.read_exact(&mut length).await?;
      let mut name = vec![0u8; usize::from(length[0])];
      client.read_exact(&mut name).await?;
      (String::from_utf8_lossy(&name).into_owned(), true)
    }
    // REP X'08': address type not supported (section 6).
    _ => return client.write_all(&[5, 0x08, 0, 1, 0, 0, 0, 0, 0, 0]).await,
  };
  let mut port = [0u8; 2];
  client.read_exact(&mut port).await?;
  seen.lock().unwrap().push(Seen {
    target: format!("{target}:{}", u16::from_be_bytes(port)),
    name,
    credentials,
  });
  if request[1] != 0x01 {
    // REP X'07': command not supported.
    return client.write_all(&[5, 0x07, 0, 1, 0, 0, 0, 0, 0, 0]).await;
  }
  let Ok(mut upstream) = TcpStream::connect(forward_to).await else {
    // REP X'05': connection refused.
    return client.write_all(&[5, 0x05, 0, 1, 0, 0, 0, 0, 0, 0]).await;
  };
  // REP X'00' with BND.ADDR 0.0.0.0:0, which the gateway does not use.
  client.write_all(&[5, 0x00, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
  tokio::io::copy_bidirectional(&mut client, &mut upstream).await.map(drop)
}
