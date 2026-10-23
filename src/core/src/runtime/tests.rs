use super::*;
#[test]
fn invalid_duration_and_embedded_credentials_fail_before_transport() {
    let mut config = RuntimeConfig::new(
        "tls://localhost:4222",
        Trust::System,
        Authentication {
            username: "peer".into(),
            password: "fixture-only".into(),
        },
        "fixture.config",
        PeerId(1),
        Membership::BrokerAuthorized,
    );
    let valid_timeout = config.peer_timeout;
    assert!(config.validate_profile(ManagerConfig::default()).is_ok());
    config.peer_timeout = Duration::MAX;
    assert_eq!(
        config.validate_profile(ManagerConfig::default()),
        Err(RuntimeError::Config)
    );
    config.peer_timeout = valid_timeout;
    assert!(config.validate_profile(ManagerConfig::default()).is_ok());
    config.url = "tls://secret-user:secret-pass@localhost:4222".into();
    let error = config
        .validate_profile(ManagerConfig::default())
        .unwrap_err();
    assert_eq!(error, RuntimeError::Config);
    assert!(!error.to_string().contains("secret"));
    let debug = format!("{config:?}");
    assert!(!debug.contains("fixture-only") && !debug.contains("secret"));
}
#[test]
fn delivery_flight_acknowledges_only_authenticated_snapshot_prefix() {
    let now = Instant::now();
    let mut session = Session {
        generation: 10,
        token: 20,
        hello: 30,
        incarnation: 1,
        tx: 9,
        rx: 0,
        next_ping: now,
        can_ping: true,
        proof_deadline: now,
        ping: Some((40, 7, now, 100)),
        delivery: DeliveryFlight {
            sent: 400,
            acknowledged: 0,
            sequence: 0,
        },
        sender_credit: delivery::Sender::default(),
        recipient_credit: delivery::Recipient::default(),
        control: delivery::ControlWindow::default(),
        pending_control: None,
        closing: None,
        advertised: delivery::Limit::default(),
        target: Amount {
            bytes: 4096,
            records: 2,
        },
        demand: 0,
        announced_demand: 0,
        remote_demand: 0,
        growth_applied: 0,
        growth_records: 0,
        last_data: now,
        fence_sent: false,
        frozen_sent: false,
        fence_deadline: None,
        pong: None,
        retiring: false,
        proven: true,
        retired_at: None,
    };
    let receipt = Control {
        kind: 6,
        sender: 10,
        recipient: 50,
        hello: 40,
        token: 20,
        watermark: 7,
        ..Control::default()
    };
    for invalid in [
        Control {
            sender: 11,
            ..receipt
        },
        Control {
            recipient: 51,
            ..receipt
        },
        Control {
            token: 21,
            ..receipt
        },
        Control {
            hello: 41,
            ..receipt
        },
        Control {
            watermark: 8,
            ..receipt
        },
    ] {
        assert!(!session.acknowledge_receipt(&invalid, 50).unwrap());
        assert_eq!(session.delivery.acknowledged, 0);
        assert!(session.ping.is_some());
    }
    assert!(session.acknowledge_receipt(&receipt, 50).unwrap());
    assert_eq!(session.delivery.sent - session.delivery.acknowledged, 300);
    assert_eq!(session.delivery.sequence, 7);
    assert!(!session.acknowledge_receipt(&receipt, 50).unwrap());
    assert_eq!(session.delivery.acknowledged, 100);
    session.ping = Some((60, 9, now, 400));
    assert!(
        session
            .acknowledge_receipt(
                &Control {
                    hello: 60,
                    watermark: 9,
                    ..receipt
                },
                50
            )
            .unwrap()
    );
    assert_eq!(session.delivery.sent - session.delivery.acknowledged, 0);
    assert!(matches!(
        session.delivery.acknowledge(8, 400),
        Err(RuntimeError::Protocol)
    ));
}
#[test]
fn recipient_flight_counts_all_sources_and_preserves_the_current_profile() {
    let limit = lane_credit_limit(128).unwrap();
    assert!(limit < DATA_DELIVERY_BYTES);
    assert!(limit + 128 * SOURCE_CONTROL_BYTES + ENVELOPE_WEIGHT <= BROKER_PENDING_BYTES);
    let mut first = delivery::Recipient::default();
    let mut second = delivery::Recipient::default();
    first
        .extend(Amount {
            bytes: limit - 1024,
            records: 100,
        })
        .unwrap();
    second
        .extend(Amount {
            bytes: 1024,
            records: 1,
        })
        .unwrap();
    assert_eq!(first.obligation().bytes + second.obligation().bytes, limit);
    first.freeze().unwrap();
    first
        .observe_frozen(
            1,
            Amount {
                bytes: 512,
                records: 1,
            },
        )
        .unwrap();
    assert!(first.reclaim().unwrap().is_none());
    first
        .apply(Amount {
            bytes: 512,
            records: 1,
        })
        .unwrap();
    first.reclaim().unwrap().unwrap();
    assert_eq!(second.obligation().bytes, 1024);
    assert!(lane_credit_limit(usize::MAX).is_err());
    assert!(std::mem::size_of::<Peer>() + std::mem::size_of::<RetiredSource>() <= 2048);
}
#[test]
fn control_and_credit_visitation_reach_every_peer_with_independent_cursors() {
    let peers: BTreeMap<_, _> = (0..128).map(|id| (PeerId(id), Peer::new())).collect();
    let (mut control, mut credit) = (Some(PeerId(7)), Some(PeerId(71)));
    let (mut controls, mut credits) = (BTreeMap::new(), BTreeMap::new());
    for _ in 0..8 {
        for id in peer_batch(&peers, &mut control, 32) {
            *controls.entry(id).or_insert(0) += 1;
        }
        for id in peer_batch(&peers, &mut credit, 32) {
            *credits.entry(id).or_insert(0) += 1;
        }
    }
    assert_eq!(controls.len(), 128);
    assert_eq!(credits.len(), 128);
    assert!(controls.values().all(|n| *n == 2));
    assert!(credits.values().all(|n| *n == 2));
}
#[test]
fn cancelled_batch_fails_every_touched_shard_and_overflow_cannot_hide_loss() {
    let a = Arc::new(Latch::default());
    let b = Arc::new(Latch::default());
    drop(BatchGuard {
        latches: vec![a.clone(), b.clone()],
        complete: false,
    });
    assert_eq!(a.get(), 4);
    assert_eq!(b.get(), 4);
    let latch = Latch::default();
    latch.mark(2);
    latch.mark(1);
    assert_eq!(latch.get(), 3);
    assert!(
        latch
            .0
            .compare_exchange(2, 0, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
    );
    assert_eq!(latch.get(), 3);
}
