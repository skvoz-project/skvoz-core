#![allow(dead_code)]
use skvoz_core_contracts::{Config, ManagerConfig, PeerId, runtime::*};
use std::{
    fs,
    net::TcpListener,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// Shared limits for the standalone workload and the native TCP relay scenarios.
pub fn runtime_scenario_profile(mut config: RuntimeConfig) -> RuntimeConfig {
    config.shards = 8;
    config.subscription_capacity = 128;
    config.join_capacity = 128;
    config.client_capacity = 16;
    config.max_incoming_per_turn = 64;
    config.max_outgoing_per_turn = 32;
    config.io_timeout = Duration::from_millis(700);
    config.heartbeat_interval = Duration::from_millis(100);
    config.peer_timeout = Duration::from_millis(800);
    config.join_timeout = Duration::from_secs(5);
    config.retry_initial = Duration::from_millis(100);
    config.retry_max = Duration::from_secs(1);
    config.max_retries = 8;
    config.terminal_drain_timeout = Duration::from_secs(5);
    config
}

pub struct Fixture {
    pub directory: PathBuf,
    pub port: u16,
    broker: Option<Child>,
}
impl Fixture {
    pub fn new() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "skvoz-core-native-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let fixture = Self {
            directory,
            port,
            broker: None,
        };
        fixture.openssl(&[
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-keyout",
            "ca.key",
            "-out",
            "ca.pem",
            "-days",
            "1",
            "-subj",
            "/CN=Core fixture CA",
            "-addext",
            "basicConstraints=critical,CA:TRUE",
        ]);
        fixture.openssl(&[
            "req",
            "-new",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-keyout",
            "leaf.key",
            "-out",
            "leaf.csr",
            "-subj",
            "/CN=server.invalid",
        ]);
        fixture.leaf(Some("IP:203.0.113.9,DNS:server.invalid"), "serverAuth", "1");
        fixture
    }
    fn openssl(&self, args: &[&str]) {
        let result = Command::new("openssl")
            .args(args)
            .current_dir(&self.directory)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "fixture certificate generation failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    pub fn leaf(&self, san: Option<&str>, eku: &str, days: &str) {
        let mut extension = format!("basicConstraints=critical,CA:FALSE\nextendedKeyUsage={eku}\n");
        if let Some(san) = san {
            extension.push_str(&format!("subjectAltName={san}\n"));
        }
        fs::write(self.directory.join("leaf.ext"), extension).unwrap();
        self.openssl(&[
            "x509",
            "-req",
            "-in",
            "leaf.csr",
            "-CA",
            "ca.pem",
            "-CAkey",
            "ca.key",
            "-CAcreateserial",
            "-out",
            "leaf.pem",
            "-days",
            days,
            "-extfile",
            "leaf.ext",
        ]);
        for entry in fs::read_dir(&self.directory).unwrap() {
            let entry = entry.unwrap();
            fs::set_permissions(entry.path(), fs::Permissions::from_mode(0o600)).unwrap();
        }
    }
    pub fn broker(&mut self) {
        let path = |name: &str| {
            serde_json::to_string(self.directory.join(name).to_str().unwrap()).unwrap()
        };
        let config = format!(
            "listen: \"127.0.0.1:{}\"\nmax_payload: 65588\ntls {{ cert_file: {}, key_file: {}, handshake_first: false }}\nauthorization {{ users: [{{user: \"p0\", password: \"fixture-only\"}},{{user: \"p1\", password: \"fixture-only\"}}] }}\n",
            self.port,
            path("leaf.pem"),
            path("leaf.key")
        );
        let config_path = self.directory.join("nats.conf");
        fs::write(&config_path, config).unwrap();
        fs::set_permissions(&config_path, fs::Permissions::from_mode(0o600)).unwrap();
        self.start();
    }
    /// Peers can publish only their authenticated sender subjects.
    pub fn runtime_broker(&mut self) {
        self.leaf(
            Some("DNS:localhost,IP:127.0.0.1,DNS:server.invalid"),
            "serverAuth",
            "1",
        );
        let users: Vec<_> = (0..=9).map(|id| format!(
            "{{user: \"p{id}\", password: \"fixture-only\", permissions: {{ publish: [\"skvoz.runtime.*.*.join.*.{id}\", \"skvoz.runtime.*.*.lane.*.*.*.*.{id}.*\", \"skvoz.static.*.*.*.*.{id}.*\"], subscribe: [\"skvoz.runtime.*.*.join.{id}.*\", \"skvoz.runtime.*.*.lane.{id}.>\", \"skvoz.static.*.*.{id}.*.*.*\"] }} }}"
        )).collect();
        let path = |name: &str| serde_json::to_string(&self.path(name)).unwrap();
        let config = format!(
            "listen: \"127.0.0.1:{}\"\nmax_payload: 65588\ntls {{ cert_file: {}, key_file: {}, handshake_first: false }}\nauthorization {{ users: [{}] }}\n",
            self.port,
            path("leaf.pem"),
            path("leaf.key"),
            users.join(",")
        );
        let path = self.path("nats.conf");
        fs::write(&path, config).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        self.start();
    }
    fn start(&mut self) {
        assert!(self.broker.is_none(), "fixture broker already running");
        let log = fs::File::create(self.path("nats.log")).unwrap();
        self.broker = Some(
            Command::new("nats-server")
                .arg("-c")
                .arg(self.path("nats.conf"))
                .stdout(Stdio::from(log.try_clone().unwrap()))
                .stderr(Stdio::from(log))
                .spawn()
                .unwrap(),
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        loop {
            let status = self.broker.as_mut().unwrap().try_wait().unwrap();
            let log = fs::read_to_string(self.path("nats.log")).unwrap_or_default();
            assert!(status.is_none(), "fixture broker exited: {status:?}\n{log}");
            if log.contains("Server is ready") {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "fixture readiness deadline\n{log}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    pub fn stop(&mut self) {
        if let Some(mut broker) = self.broker.take() {
            broker.kill().unwrap();
            broker.wait().unwrap();
        }
    }
    pub fn restart(&mut self) {
        self.stop();
        self.start();
    }
    pub fn reload(&mut self) {
        let offset = fs::metadata(self.path("nats.log")).unwrap().len() as usize;
        let pid = self.broker.as_ref().unwrap().id();
        let result = Command::new("nats-server")
            .args(["--signal", &format!("reload={pid}")])
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "fixture reload failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        loop {
            let log = fs::read_to_string(self.path("nats.log")).unwrap();
            assert!(
                self.broker.as_mut().unwrap().try_wait().unwrap().is_none(),
                "fixture broker exited\n{log}"
            );
            if log[offset..].contains("Reloaded server configuration") {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "fixture reload deadline\n{log}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    pub fn config(&self, id: u64) -> RuntimeConfig {
        let mut config = RuntimeConfig::new(
            format!("tls://127.0.0.1:{}", self.port),
            Trust::ManagedCa(self.directory.join("ca.pem")),
            Authentication {
                username: format!("p{id}"),
                password: "fixture-only".into(),
            },
            "fixture.native",
            PeerId(id),
            Membership::BrokerAuthorized,
        );
        config.tls_server_name = Some("server.invalid".into());
        config.shards = 1;
        config.subscription_capacity = 8;
        config.join_capacity = 8;
        config.client_capacity = 4;
        config.io_timeout = Duration::from_secs(1);
        config.join_timeout = Duration::from_secs(3);
        config.heartbeat_interval = Duration::from_millis(100);
        config.peer_timeout = Duration::from_secs(2);
        config.terminal_drain_timeout = Duration::from_secs(1);
        if id == 1 {
            config.initiate = vec![PeerId(0)];
        }
        config
    }
    pub fn limits() -> ManagerConfig {
        ManagerConfig {
            stream: Config {
                max_metadata: 512,
                max_frame: 16384,
                receive_window: 65536,
                max_pending_frames: 16,
                open_timeout_ms: 3000,
            },
            max_peers: 2,
            max_streams: 8,
            max_streams_per_peer: 8,
            receive_budget: 1 << 20,
            receive_budget_per_peer: 1 << 20,
            send_budget: 1 << 20,
            send_budget_per_peer: 1 << 20,
        }
    }
    pub fn path(&self, name: &str) -> PathBuf {
        self.directory.join(name)
    }
    pub fn wrong_root(&self) -> PathBuf {
        self.openssl(&[
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-keyout",
            "wrong.key",
            "-out",
            "wrong.pem",
            "-days",
            "1",
            "-subj",
            "/CN=Unrelated fixture CA",
            "-addext",
            "basicConstraints=critical,CA:TRUE",
        ]);
        self.path("wrong.pem")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(mut broker) = self.broker.take() {
            let _ = broker.kill();
            let _ = broker.wait();
        }
        let _ = fs::remove_dir_all(&self.directory);
    }
}
pub fn private_json(path: &Path, value: &impl serde::Serialize) {
    fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}
