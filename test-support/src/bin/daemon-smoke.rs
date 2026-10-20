//! Standalone fixture probe also runs on the minimum supported libc without libtest.
use skvoz_core_test_support::{Fixture, private_json};
use std::{
    fs,
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let binary = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("daemon executable required")?,
    );
    let mut fixture = Fixture::new();
    fixture.broker();
    let socket = fixture.path("core.sock");
    let profile = fixture.path("profile.json");
    private_json(
        &profile,
        &serde_json::json!({"ipc_path":socket,"url":format!("tls://127.0.0.1:{}",fixture.port),
        "tls_server_name":"server.invalid","trust":"managed_ca","ca_file":fixture.path("ca.pem"),
        "username":"p0","password":"fixture-only","namespace":"fixture.daemon","peer_id":0,
        "allowed_peers":[1],"initiate":[]}),
    );
    let log_path = fixture.path("daemon.log");
    let log = fs::File::create(&log_path)?;
    let mut process = Process(
        Command::new(binary)
            .args(["--config"])
            .arg(profile)
            .env_remove("LD_LIBRARY_PATH")
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log))
            .spawn()?,
    );
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut stream = loop {
        if let Ok(stream) = UnixStream::connect(&socket) {
            break stream;
        }
        if Instant::now() >= deadline || process.0.try_wait()?.is_some() {
            return Err(format!(
                "daemon readiness failed: {}",
                fs::read_to_string(&log_path)?
            )
            .into());
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut hello = 37u32.to_be_bytes().to_vec();
    hello.extend_from_slice(b"SKI1");
    hello.extend_from_slice(&1u16.to_be_bytes());
    hello.extend_from_slice(&1u16.to_be_bytes());
    hello.extend_from_slice(&1u64.to_be_bytes());
    hello.extend_from_slice(&0u128.to_be_bytes());
    hello.extend_from_slice(&[0, 1, 0, 1, 0]);
    stream.write_all(&hello)?;
    let mut prefix = [0; 4];
    stream.read_exact(&mut prefix)?;
    let size = u32::from_be_bytes(prefix) as usize;
    if !(32..=65568).contains(&size) {
        return Err("invalid IPC smoke response bound".into());
    }
    let mut body = vec![0; size];
    stream.read_exact(&mut body)?;
    if &body[..4] != b"SKI1"
        || u16::from_be_bytes(body[6..8].try_into()?) != 0x8000
        || body[32..34] != [0, 0]
    {
        return Err("daemon HELLO smoke failed".into());
    }
    if fs::read_to_string(log_path)?.contains("fixture-only") {
        return Err("daemon exposed fixture credentials".into());
    }
    println!("Core binary daemon TLS connection and IPC HELLO passed");
    Ok(())
}
