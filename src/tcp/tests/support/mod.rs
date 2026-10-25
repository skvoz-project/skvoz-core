pub type BenchError = Box<dyn std::error::Error + Send + Sync>;
pub mod runtime_scenarios;

/// Provision a private child environment without mutating process-global env.
pub fn provision(test: &str) -> bool {
    if std::env::var("SKVOZ_CORE_TCP_CHILD").as_deref() == Ok("1") {
        return true;
    }
    let mut fixture = skvoz_core_test_support::Fixture::new();
    fixture.leaf(Some("IP:127.0.0.1,DNS:localhost"), "serverAuth", "1");
    fixture.broker();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env("SKVOZ_CORE_TCP_CHILD", "1")
        .env(
            "SKVOZ_NATS_URL",
            format!("tls://127.0.0.1:{}", fixture.port),
        )
        .env("SKVOZ_NATS_CA", fixture.path("ca.pem"))
        .env("SKVOZ_NATS_P0_PASSWORD", "fixture-only")
        .env("SKVOZ_NATS_P1_PASSWORD", "fixture-only")
        .env("SKVOZ_NATS_RUN_TOKEN", "native_tcp")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "native TCP child failed: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    false
}
