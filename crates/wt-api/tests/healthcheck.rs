//! `wt-api --healthcheck` (docs/adr-edge-cost.md, Tunnel follow-on): a 200 passes, anything else
//! fails, and nothing listening fails within the timeout. Servers here are loopback threads.
#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::time::Duration;

use wt_api::healthcheck::{ProbeError, probe, target};

/// Answers one connection with `reply` and returns the request it read.
fn serve_once(reply: &'static str) -> (SocketAddr, std::thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 256];
        let n = stream.read(&mut request).unwrap();
        stream.write_all(reply.as_bytes()).unwrap();
        String::from_utf8_lossy(&request[..n]).into_owned()
    });
    (addr, handle)
}

#[test]
fn a_200_passes_and_the_request_is_get_healthz() {
    let (addr, server) =
        serve_once("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\r\n{\"status\":\"ok\"}");
    probe(addr, Duration::from_secs(2)).unwrap();
    assert!(
        server
            .join()
            .unwrap()
            .starts_with("GET /healthz HTTP/1.1\r\n")
    );
}

#[test]
fn a_503_fails() {
    let (addr, server) = serve_once("HTTP/1.1 503 Service Unavailable\r\n\r\n");
    let error = probe(addr, Duration::from_secs(2)).unwrap_err();
    assert!(
        matches!(error, ProbeError::Status(ref line) if line.contains("503")),
        "{error}"
    );
    server.join().unwrap();
}

#[test]
fn nothing_listening_fails() {
    let addr = {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap()
    };
    let error = probe(addr, Duration::from_secs(2)).unwrap_err();
    assert!(matches!(error, ProbeError::Io { .. }), "{error}");
}

#[test]
fn the_target_is_the_bind_port_on_loopback() {
    let at = |s: &str| -> SocketAddr { s.parse().unwrap() };
    assert_eq!(target(at("0.0.0.0:8080")), at("127.0.0.1:8080"));
    assert_eq!(target(at("[::]:9000")), at("[::1]:9000"));
    assert_eq!(target(at("10.0.0.5:8080")), at("10.0.0.5:8080"));
}
