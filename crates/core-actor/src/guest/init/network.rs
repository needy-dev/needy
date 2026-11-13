//! The actor's network: the loopback interface, and the forwarder from the
//! in-guest proxy port to the actor gateway on the host.

use std::os::fd::AsRawFd;

use nix::net::if_::InterfaceFlags;
use nix::sys::socket::{AddressFamily, SockFlag, SockType};

use crate::contract::GATEWAY_RESET;

/// Where the driving program's `HTTP_PROXY` points.
// 3128 is Squid's default HTTP proxy port, which proxy-aware programs expect:
// https://github.com/squid-cache/squid/blob/SQUID_7_8/src/Common.am#L14
// Reference: https://github.com/squid-cache/squid/blob/SQUID_7_8/src/cf.data.pre#L2620-L2621
pub(super) const GATEWAY_LISTEN: &str = "127.0.0.1:3128";

// NOTICE: nix wraps neither `SIOCGIFFLAGS` nor `SIOCSIFFLAGS`; its ioctl
// macros do, around libc's `ifreq`. A netlink library would be the larger
// alternative for one flag.
nix::ioctl_read_bad!(get_flags, libc::SIOCGIFFLAGS, libc::ifreq);
nix::ioctl_write_ptr_bad!(set_flags, libc::SIOCSIFFLAGS, libc::ifreq);

/// Brings `lo` up, as `ip link set lo up` does.
pub(super) fn loopback_up() -> nix::Result<()> {
  let socket = nix::sys::socket::socket(AddressFamily::Inet, SockType::Datagram, SockFlag::SOCK_CLOEXEC, None)?;
  // SAFETY: `ifreq` is plain data; all zeroes is a valid empty request.
  let mut request: libc::ifreq = unsafe { std::mem::zeroed() };
  for (slot, byte) in request.ifr_name.iter_mut().zip(b"lo\0") {
    *slot = *byte as libc::c_char;
  }
  // SAFETY: `request` names an interface and outlives both calls; the
  // kernel fills and reads `ifru_flags`, the union member both use.
  unsafe {
    get_flags(socket.as_raw_fd(), &mut request)?;
    request.ifr_ifru.ifru_flags |= InterfaceFlags::IFF_UP.bits() as libc::c_short;
    set_flags(socket.as_raw_fd(), &request)?;
  }
  Ok(())
}

/// Where the host serves the actor gateway.
#[derive(Debug, Clone)]
pub(super) enum Gateway {
  /// A vsock port on the host (a VM's).
  Vsock(u32),
  /// A Unix socket bind-mounted into the actor (a container's).
  Unix(std::path::PathBuf),
}

/// Forwards the in-guest proxy port to the actor gateway.
pub(super) async fn forward_gateway(gateway: Gateway) {
  if matches!(gateway, Gateway::Vsock(0)) {
    return;
  }
  let listener = match tokio::net::TcpListener::bind(GATEWAY_LISTEN).await {
    Ok(listener) => listener,
    Err(e) => {
      tracing::warn!("cannot listen on {GATEWAY_LISTEN}; the driving program has no network: {e}");
      return;
    }
  };
  loop {
    let inbound = match listener.accept().await {
      Ok((inbound, _)) => inbound,
      Err(e) => {
        tracing::warn!("the gateway forwarder stops: {e}");
        return;
      }
    };
    let gateway = gateway.clone();
    tokio::spawn(async move {
      let copied = match &gateway {
        Gateway::Vsock(port) => {
          // CID 2 is the host, `VMADDR_CID_HOST`: https://github.com/torvalds/linux/blob/v6.18/include/uapi/linux/vm_sockets.h#L126-L131
          match tokio_vsock::VsockStream::connect(tokio_vsock::VsockAddr::new(2, *port)).await {
            Ok(outbound) => relay(inbound, outbound).await,
            Err(e) => {
              tracing::warn!("cannot reach the gateway on vsock port {port}: {e}");
              return;
            }
          }
        }
        Gateway::Unix(path) => match tokio::net::UnixStream::connect(path).await {
          Ok(outbound) => relay(inbound, outbound).await,
          Err(e) => {
            tracing::warn!("cannot reach the gateway at {}: {e}", path.display());
            return;
          }
        },
      };
      if let Err(e) = copied {
        tracing::debug!("gateway connection: {e}");
      }
    });
  }
}

/// Relays the driving program's connection `inbound` and the gateway's
/// `outbound` both ways until both ends closed, as
/// `tokio::io::copy_bidirectional` does, except that a gateway that starts
/// with [`GATEWAY_RESET`] resets the program's connection (`ECONNRESET`
/// for the program), which is how the gateway delivers a `reset` fault.
pub(super) async fn relay<O>(inbound: tokio::net::TcpStream, outbound: O) -> std::io::Result<()>
where
  O: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
  use tokio::io::{AsyncReadExt, AsyncWriteExt};
  let (mut from_program, mut to_program) = inbound.into_split();
  let (mut from_gateway, mut to_gateway) = tokio::io::split(outbound);
  let up = async {
    let copied = tokio::io::copy(&mut from_program, &mut to_gateway).await;
    // The program's end of stream passes on as one; a gateway gone already
    // needs none.
    let _ = to_gateway.shutdown().await;
    copied.map(drop)
  };
  let down = async {
    // The gateway's first bytes, until they can no longer be the marker.
    let mut start = Vec::with_capacity(GATEWAY_RESET.len());
    while start.len() < GATEWAY_RESET.len() && GATEWAY_RESET.starts_with(&start) {
      let mut byte = [0u8; 64];
      let want = GATEWAY_RESET.len() - start.len();
      let n = from_gateway.read(&mut byte[..want]).await?;
      if n == 0 {
        break;
      }
      start.extend_from_slice(&byte[..n]);
    }
    if start == GATEWAY_RESET {
      return Ok(true);
    }
    to_program.write_all(&start).await?;
    tokio::io::copy(&mut from_gateway, &mut to_program).await?;
    // As with the program's end of stream, the other way.
    let _ = to_program.shutdown().await;
    Ok::<_, std::io::Error>(false)
  };
  // Boxed, so they can be dropped, and their borrows of the halves end,
  // before a reset reunites the program's connection.
  let mut up = Box::pin(up);
  let mut down = Box::pin(down);
  let mut up_done = None;
  let reset = loop {
    tokio::select! {
      copied = &mut up, if up_done.is_none() => up_done = Some(copied),
      reset = &mut down => break reset?,
    }
  };
  if reset {
    drop((up, down));
    // SO_LINGER 0 makes the close send RST instead of FIN: the program
    // reads a reset connection, as from a real upstream's reset.
    let stream = from_program.reunite(to_program).map_err(|e| std::io::Error::other(e.to_string()))?;
    let linger = libc::linger {
      l_onoff: 1,
      l_linger: 0,
    };
    nix::sys::socket::setsockopt(&stream, nix::sys::socket::sockopt::Linger, &linger).map_err(std::io::Error::from)?;
    return Ok(());
  }
  match up_done {
    Some(copied) => copied,
    None => up.await,
  }
}

#[cfg(test)]
#[path = "network_test.rs"]
mod tests;
