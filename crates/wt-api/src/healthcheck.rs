//! `wt-api --healthcheck`: the web container's ECS health check (`docs/adr-edge-cost.md`, Tunnel
//! follow-on). The image is distroless, with no shell or `curl`, so the binary checks itself: one
//! `GET /healthz` to its own port on the loopback address, and success only on a 200.

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::time::Duration;

/// How long the connection, the request and the reply each may take. ECS gives the whole check
/// five seconds.
pub const TIMEOUT: Duration = Duration::from_secs(3);

/// Why the check failed.
#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    /// `WT_BIND` is not a socket address.
    #[error("WT_BIND is not an address and port: {0:?}")]
    Bind(String),
    /// The server could not be reached or did not answer in time.
    #[error("no answer from {addr}: {source}")]
    Io {
        /// Where the check connected.
        addr: SocketAddr,
        /// The connection's error.
        source: std::io::Error,
    },
    /// The server answered with something other than a 200.
    #[error("/healthz answered {0:?}")]
    Status(String),
}

/// Where to check: the port of `WT_BIND` (default `127.0.0.1:8080`, as `wt_core::config`) on the
/// loopback address of the same family, since an unspecified address cannot be connected to.
///
/// # Errors
///
/// [`ProbeError::Bind`] when the value is not an address and port.
pub fn target(bind: Option<&str>) -> Result<SocketAddr, ProbeError> {
    let bind = bind.unwrap_or("127.0.0.1:8080");
    let addr: SocketAddr = bind
        .parse()
        .map_err(|_| ProbeError::Bind(bind.to_owned()))?;
    let ip = match addr.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        ip => ip,
    };
    Ok(SocketAddr::new(ip, addr.port()))
}

/// Sends `GET /healthz` to `addr` and succeeds only on a 200.
///
/// # Errors
///
/// [`ProbeError::Io`] when nothing answers in time, [`ProbeError::Status`] on any other status.
pub fn probe(addr: SocketAddr, timeout: Duration) -> Result<(), ProbeError> {
    let io = |source| ProbeError::Io { addr, source };
    let mut stream = TcpStream::connect_timeout(&addr, timeout).map_err(io)?;
    stream.set_read_timeout(Some(timeout)).map_err(io)?;
    stream.set_write_timeout(Some(timeout)).map_err(io)?;
    stream
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .map_err(io)?;
    // The status line is all that matters, and it fits in the first read.
    let mut head = [0_u8; 64];
    let n = stream.read(&mut head).map_err(io)?;
    let line = String::from_utf8_lossy(&head[..n]);
    let line = line.lines().next().unwrap_or_default();
    if line.split(' ').nth(1) == Some("200") {
        Ok(())
    } else {
        Err(ProbeError::Status(line.to_owned()))
    }
}
