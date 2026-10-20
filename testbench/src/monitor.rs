//! Bounded local fixture HTTP observation, without a foreign-language harness.
use crate::BenchError;
use std::{
    io::{Read, Write},
    net::TcpStream,
    time::{Duration, Instant},
};
pub fn get(path: &str) -> Result<serde_json::Value, BenchError> {
    let base = std::env::var("SKVOZ_NATS_MONITOR")?;
    let address = base
        .strip_prefix("http://")
        .ok_or("fixture monitor must use local HTTP")?;
    let mut socket = TcpStream::connect(address)?;
    socket.set_read_timeout(Some(Duration::from_secs(2)))?;
    socket.set_write_timeout(Some(Duration::from_secs(2)))?;
    write!(
        socket,
        "GET {path} HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )?;
    let mut bytes = Vec::new();
    socket.take(1 << 20).read_to_end(&mut bytes)?;
    let split = bytes
        .windows(4)
        .position(|b| b == b"\r\n\r\n")
        .ok_or("invalid fixture HTTP response")?;
    if !bytes.starts_with(b"HTTP/1.0 200") && !bytes.starts_with(b"HTTP/1.1 200") {
        return Err("fixture HTTP status failed".into());
    }
    Ok(serde_json::from_slice(&bytes[split + 4..])?)
}
pub fn ready(timeout: Duration) -> Result<(), BenchError> {
    let deadline = Instant::now() + timeout;
    loop {
        if get("/healthz").is_ok() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err("fixture readiness deadline expired".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
