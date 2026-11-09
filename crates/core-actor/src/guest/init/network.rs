//! The actor's network: the loopback interface, and the forwarder from the
//! in-guest proxy port to the actor gateway on the host.

use std::os::fd::AsRawFd;

use nix::net::if_::InterfaceFlags;
use nix::sys::socket::{AddressFamily, SockFlag, SockType};

/// Where the driving program's `HTTP_PROXY` points.
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

/// Forwards the in-guest proxy port to the actor gateway, which the host
/// serves on vsock port `port`.
pub(super) async fn forward_gateway(port: u32) {
  if port == 0 {
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
    let mut inbound = match listener.accept().await {
      Ok((inbound, _)) => inbound,
      Err(e) => {
        tracing::warn!("the gateway forwarder stops: {e}");
        return;
      }
    };
    tokio::spawn(async move {
      let mut outbound = match tokio_vsock::VsockStream::connect(tokio_vsock::VsockAddr::new(2, port)).await {
        Ok(outbound) => outbound,
        Err(e) => {
          tracing::warn!("cannot reach the gateway on vsock port {port}: {e}");
          return;
        }
      };
      if let Err(e) = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await {
        tracing::debug!("gateway connection: {e}");
      }
    });
  }
}
