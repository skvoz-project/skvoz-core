//! Real native-library regression, including the INFO -> verified TLS -> CONNECT boundary.
#![cfg(target_os = "linux")]
use skvoz_core_binding::{CloseReason, Event, PeerId, SendOutcome, runtime::*};
use skvoz_core_test_support as support;
use std::time::{Duration, Instant};
use support::Fixture;

async fn drive(a: &mut NatsRuntime, b: &mut NatsRuntime) {
    a.turn(Duration::ZERO).await.unwrap();
    b.turn(Duration::from_millis(1)).await.unwrap();
}
async fn joined(a: &mut NatsRuntime, b: &mut NatsRuntime) {
    let deadline = Instant::now() + Duration::from_secs(4);
    while !a.peer_ready(PeerId(0)) || !b.peer_ready(PeerId(1)) {
        assert!(Instant::now() < deadline, "native peer join deadline");
        drive(a, b).await;
    }
}
async fn opened(a: &mut NatsRuntime, b: &mut NatsRuntime) -> (RuntimeKey, RuntimeKey) {
    let local = a.open(PeerId(0), &[0, 255, 128]).unwrap();
    let mut remote = None;
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        assert!(Instant::now() < deadline, "native stream open deadline");
        drive(a, b).await;
        for event in b.poll_events(256) {
            if let Event::IncomingOpen { metadata } = event.event {
                assert_eq!(&*metadata, &[0, 255, 128]);
                b.accept(event.key, b"accepted").unwrap();
                remote = Some(event.key);
            }
        }
        if a.poll_events(256)
            .iter()
            .any(|event| matches!(event.event, Event::Opened { .. }))
        {
            return (local, remote.unwrap());
        }
    }
}
#[tokio::test(flavor = "current_thread")]
async fn native_order_admission_half_close_wake_and_cancel() {
    let mut fixture = Fixture::new();
    fixture.broker();
    let mut a = NatsRuntime::connect(fixture.config(1), Fixture::limits())
        .await
        .unwrap();
    let mut b = NatsRuntime::connect(fixture.config(0), Fixture::limits())
        .await
        .unwrap();
    joined(&mut a, &mut b).await;
    assert!(a.epoch() != 0);
    assert!(a.delivery_status().is_ok());
    b.set_diagnostics_enabled(true);
    let (local, remote) = opened(&mut a, &mut b).await;
    let payload: Vec<_> = (0..32768).map(|i| (i * 37) as u8).collect();
    let mut sent = 0;
    let mut received = Vec::new();
    let mut admission_panicked = false;
    let deadline = Instant::now() + Duration::from_secs(4);
    while received.len() < payload.len() {
        assert!(Instant::now() < deadline, "native transfer deadline");
        if sent < payload.len() {
            if let SendOutcome::Accepted(n) = a.send(local, &payload[sent..]).unwrap() {
                sent += n;
            }
        }
        drive(&mut a, &mut b).await;
        if !admission_panicked && b.resources().receive_unconsumed_bytes != 0 {
            let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                b.poll_events_with_data_budget(256, |_, _| {
                    panic!("fixture admission callback panic")
                })
            }));
            assert!(panic.is_err());
            admission_panicked = true;
        }
        // A denied frame remains owned by Core rather than being acknowledged.
        assert!(
            b.poll_events_with_data_budget(256, |_, _| false)
                .iter()
                .all(|e| !matches!(e.event, Event::Data { .. }))
        );
        for event in b.poll_events(256) {
            if let Event::Data { offset, bytes } = event.event {
                assert_eq!(offset as usize, received.len());
                received.extend_from_slice(&bytes);
                drop(bytes);
                b.consume_through(remote, received.len() as u64).unwrap();
            }
        }
        a.poll_events(256);
    }
    assert!(admission_panicked);
    assert_eq!(received, payload);
    a.finish(local).unwrap();
    let mut fin = false;
    while !fin {
        assert!(Instant::now() < deadline, "native FIN deadline");
        drive(&mut a, &mut b).await;
        fin = b
            .poll_events(256)
            .iter()
            .any(|e| matches!(e.event, Event::RemoteFinished));
        a.poll_events(256);
    }
    assert!(matches!(
        b.send(remote, b"after-fin").unwrap(),
        SendOutcome::Accepted(9)
    ));
    b.finish(remote).unwrap();
    let mut response = Vec::new();
    let mut remote_fin = false;
    while !remote_fin {
        assert!(Instant::now() < deadline, "native response deadline");
        drive(&mut a, &mut b).await;
        for event in a.poll_events(256) {
            match event.event {
                Event::Data { offset, bytes } => {
                    response.extend_from_slice(&bytes);
                    let end = offset + bytes.len() as u64;
                    drop(bytes);
                    a.consume_through(local, end).unwrap();
                }
                Event::RemoteFinished => remote_fin = true,
                _ => {}
            }
        }
        b.poll_events(256);
    }
    assert_eq!(response, b"after-fin");
    for _ in 0..4 {
        drive(&mut a, &mut b).await;
        a.poll_events(256);
        b.poll_events(256);
    }
    let host_polled = std::cell::Cell::new(false);
    for _ in 0..8 {
        let host = std::future::poll_fn(|_| {
            host_polled.set(true);
            std::task::Poll::Ready(())
        });
        a.turn_with_wake(Duration::from_secs(1), host)
            .await
            .unwrap();
        if host_polled.get() {
            break;
        }
    }
    assert!(
        host_polled.get(),
        "native host wake was never polled at idle"
    );
    // Polling/cancelling an idle operation releases the exclusive handle.
    let mut pending = false;
    for _ in 0..8 {
        {
            let future = a.turn(Duration::from_secs(1));
            let mut future = std::pin::pin!(future);
            pending = std::future::poll_fn(|cx| {
                std::task::Poll::Ready(std::future::Future::poll(future.as_mut(), cx).is_pending())
            })
            .await;
        }
        if pending {
            break;
        }
    }
    assert!(
        pending,
        "native idle operation never reached pending cancellation"
    );
    // A distinct explicit cancellation is still delivered through the same engine.
    let (cancelled, _) = opened(&mut a, &mut b).await;
    a.close(cancelled).unwrap();
    assert!(a.poll_events(256).iter().any(|e| matches!(
        e.event,
        Event::Closed {
            reason: CloseReason::Cancelled
        }
    )));
    assert!(b.diagnostics().turns > 0);
    a.transport_lost();
    b.transport_lost();
    a.shutdown().await.unwrap();
    b.shutdown().await.unwrap();
}
#[tokio::test(flavor = "current_thread")]
async fn native_verified_tls_matrix() {
    let mut fixture = Fixture::new();
    fixture.broker();
    for name in ["server.invalid", "203.0.113.9"] {
        let mut config = fixture.config(1);
        config.tls_server_name = Some(name.into());
        let mut runtime = NatsRuntime::connect(config, Fixture::limits())
            .await
            .unwrap();
        runtime.shutdown().await.unwrap();
    }
    let mut cases = Vec::new();
    let mut config = fixture.config(1);
    config.tls_server_name = None;
    cases.push(config);
    let mut config = fixture.config(1);
    config.tls_server_name = Some("wrong.invalid".into());
    cases.push(config);
    let mut config = fixture.config(1);
    config.trust = Trust::ManagedCa(fixture.wrong_root());
    cases.push(config);
    std::fs::write(fixture.path("empty.pem"), b"not PEM").unwrap();
    let mut config = fixture.config(1);
    config.trust = Trust::ManagedCa(fixture.path("empty.pem"));
    cases.push(config);
    let mut config = fixture.config(1);
    config.trust = Trust::System;
    cases.push(config);
    for config in cases {
        assert!(matches!(
            NatsRuntime::connect(config, Fixture::limits()).await,
            Err(RuntimeError::Tls)
        ));
    }
    let mut config = fixture.config(1);
    config.tls_server_name = Some("[::1]".into());
    assert!(matches!(
        NatsRuntime::connect(config, Fixture::limits()).await,
        Err(RuntimeError::Config)
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn native_rejects_expired_cn_only_and_wrong_eku() {
    for (san, eku, days) in [
        (Some("DNS:server.invalid"), "serverAuth", "-1"),
        (None, "serverAuth", "1"),
        (Some("DNS:server.invalid"), "clientAuth", "1"),
    ] {
        let mut fixture = Fixture::new();
        fixture.leaf(san, eku, days);
        fixture.broker();
        assert!(matches!(
            NatsRuntime::connect(fixture.config(1), Fixture::limits()).await,
            Err(RuntimeError::Tls)
        ));
    }
}
#[test]
fn native_system_roots_child() {
    let Ok(path) = std::env::var("SKVOZ_CORE_SYSTEM_PROFILE") else {
        return;
    };
    let config: RuntimeConfig = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let mut runtime = NatsRuntime::connect(config, Fixture::limits())
                .await
                .unwrap();
            runtime.shutdown().await.unwrap();
        });
}
#[test]
fn native_system_roots_keep_verified_ip_identity() {
    let mut fixture = Fixture::new();
    fixture.broker();
    let mut config = fixture.config(1);
    config.trust = Trust::System;
    config.tls_server_name = Some("203.0.113.9".into());
    let profile = fixture.path("system-profile.json");
    support::private_json(&profile, &config);
    let empty = fixture.path("empty-roots");
    std::fs::create_dir(&empty).unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "native_system_roots_child", "--nocapture"])
        .env("SKVOZ_CORE_SYSTEM_PROFILE", &profile)
        .env("SSL_CERT_FILE", fixture.path("ca.pem"))
        .env("SSL_CERT_DIR", empty)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "native System trust child failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn host_callback_panic_is_contained_before_crossing_c_abi() {
    use futures_util::FutureExt;
    let mut fixture = Fixture::new();
    fixture.broker();
    let mut node = NatsRuntime::connect(fixture.config(0), Fixture::limits())
        .await
        .unwrap();
    let mut caught = false;
    for _ in 0..8 {
        let result =
            std::panic::AssertUnwindSafe(node.turn_with_wake(Duration::from_secs(1), async {
                panic!("fixture host callback panic")
            }))
            .catch_unwind()
            .await;
        if result.is_err() {
            caught = true;
            break;
        }
    }
    assert!(caught);
    assert_eq!(node.status().lifecycle, Lifecycle::Ready);
    node.shutdown().await.unwrap();
}
