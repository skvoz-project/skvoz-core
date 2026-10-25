//! The public fixed-route API keeps its own session and terminal contract.
mod support;
use skvoz_core::{
    CloseReason, Event, Frame, ManagerConfig, PeerId, SendOutcome, StreamKey,
    nats::{NatsConfig, NatsNode, PeerRoute},
    wire,
};
use std::time::{Duration, Instant};

fn config(fixture: &support::Fixture, id: u64) -> NatsConfig {
    NatsConfig {
        url: fixture.config(id).url,
        ca: fixture.path("ca.pem"),
        username: format!("p{id}"),
        password: "fixture-only".into(),
        namespace: "skvoz.static.fixture.routes".into(),
        id: PeerId(id),
        session: format!("g{id}"),
        peers: vec![PeerRoute {
            id: PeerId(1 - id),
            session: format!("g{}", 1 - id),
        }],
        name: format!("static-{id}"),
        subscription_capacity: 8,
        client_capacity: 8,
        max_outgoing_per_turn: 8,
        max_incoming_per_turn: 8,
        io_timeout: Duration::from_secs(2),
    }
}
async fn open(a: &mut NatsNode, b: &mut NatsNode) -> StreamKey {
    // Admit initial peer controls before opening into the one-record fixture.
    let controls_until = Instant::now() + Duration::from_secs(3);
    loop {
        assert!(
            Instant::now() < controls_until,
            "initial static controls did not drain"
        );
        let a_progress = a.turn(Duration::from_millis(10)).await.unwrap();
        let b_progress = b.turn(Duration::from_millis(10)).await.unwrap();
        if a_progress == 0 && b_progress == 0 {
            break;
        }
    }
    let key = a.open(PeerId(0), b"opaque").unwrap();
    let until = Instant::now() + Duration::from_secs(3);
    loop {
        assert!(Instant::now() < until, "static handshake deadline");
        a.turn(Duration::ZERO).await.unwrap();
        b.turn(Duration::from_millis(1)).await.unwrap();
        for event in b.poll_events(8) {
            if matches!(event.event, Event::IncomingOpen { .. }) {
                b.accept(event.key, b"").unwrap();
            }
        }
        if a.poll_events(8)
            .iter()
            .any(|event| event.key == key && matches!(event.event, Event::Opened { .. }))
        {
            return key;
        }
    }
}
#[tokio::test]
async fn static_routes_sessions_order_and_terminal_errors() {
    let mut fixture = support::fixture();
    let mut limits = ManagerConfig::default();
    limits.stream.receive_window = 8;
    limits.stream.max_frame = 4;
    let mut server = NatsNode::connect(config(&fixture, 0), limits)
        .await
        .unwrap();
    let mut client = NatsNode::connect(config(&fixture, 1), limits)
        .await
        .unwrap();
    let packet = wire::encode(
        3,
        &Frame::Open {
            receive_window: 8,
            max_frame: 4,
            metadata: Box::new([]),
        },
    )
    .unwrap();
    let before = server.resources();
    for subject in [
        "skvoz.static.other.routes.0.g0.1.g1",
        "skvoz.static.fixture.routes.0.old.1.g1",
        "skvoz.static.fixture.routes.0.g0.1.old",
    ] {
        client
            .inject_subject(subject.into(), packet.clone())
            .await
            .unwrap();
        server.turn(Duration::from_millis(20)).await.unwrap();
        assert_eq!(server.resources().streams, 0);
        assert_eq!(
            server.resources().reserved_receive_bytes,
            before.reserved_receive_bytes
        );
    }
    let key = open(&mut client, &mut server).await;
    let payload = [0, 255, 128, 4, 0, 1, 3, 8, 255, 17, 19, 32];
    let (mut sent, mut received) = (0, Vec::new());
    let until = Instant::now() + Duration::from_secs(3);
    while received.len() < payload.len() {
        assert!(Instant::now() < until, "static transfer deadline");
        if sent < payload.len()
            && let SendOutcome::Accepted(n) = client.send(key, &payload[sent..]).unwrap()
        {
            sent += n;
        }
        client.turn(Duration::ZERO).await.unwrap();
        server.turn(Duration::from_millis(1)).await.unwrap();
        for event in server.poll_events(8) {
            if let Event::Data { offset, bytes } = event.event {
                assert_eq!(offset, received.len() as u64);
                received.extend_from_slice(&bytes);
                drop(bytes);
                server
                    .consume_through(event.key, received.len() as u64)
                    .unwrap();
            }
        }
        client.poll_events(8);
    }
    assert_eq!(received, payload);
    let mut packet = wire::encode(key.stream_id, &Frame::WindowUpdate { consumed: 0 }).unwrap();
    packet[4] = 255;
    client.inject_packet(PeerId(0), packet).await.unwrap();
    let until = Instant::now() + Duration::from_secs(2);
    loop {
        assert!(
            Instant::now() < until,
            "malformed packet did not fail the affected stream"
        );
        if server.turn(Duration::from_millis(10)).await.is_err() {
            break;
        }
    }
    let events = server.poll_events(8);
    assert_eq!(events.len(), 1);
    assert!(matches!(
        events[0].event,
        Event::Closed {
            reason: CloseReason::ProtocolError
        }
    ));
    assert!(server.poll_events(8).is_empty());
    assert!(server.send(events[0].key, b"revive").is_err());
    assert_eq!(server.resources().buffered_receive_bytes, 0);
    fixture.stop();
    let until = Instant::now() + Duration::from_secs(3);
    while client.failure_kind().is_none() {
        assert!(
            Instant::now() < until,
            "broker disconnect did not terminate static lifetime"
        );
        let _ = client.turn(Duration::from_millis(10)).await;
    }
    assert!(client.open(PeerId(0), b"").is_err());
    fixture.restart();
    let _ = client.turn(Duration::ZERO).await;
    assert!(client.open(PeerId(0), b"revive").is_err());
    drop(server);
    drop(client);
}

#[tokio::test]
async fn static_subscription_and_permission_callbacks_fail_closed() {
    use skvoz_core::nats::FailureKind;
    for expected in [FailureKind::SlowConsumer, FailureKind::ServerError] {
        let fixture = support::fixture();
        if expected == FailureKind::ServerError {
            let mut invalid = config(&fixture, 0);
            invalid.password = "intentionally-wrong".into();
            assert!(
                NatsNode::connect(invalid, ManagerConfig::default())
                    .await
                    .is_err()
            );
            let mut invalid = config(&fixture, 0);
            invalid.ca = fixture.wrong_root();
            assert!(
                NatsNode::connect(invalid, ManagerConfig::default())
                    .await
                    .is_err()
            );
        }
        let mut connection = config(&fixture, 0);
        connection.subscription_capacity = if expected == FailureKind::SlowConsumer {
            1
        } else {
            8
        };
        let mut server = NatsNode::connect(connection, ManagerConfig::default())
            .await
            .unwrap();
        let mut client = NatsNode::connect(config(&fixture, 1), ManagerConfig::default())
            .await
            .unwrap();
        let key = open(&mut client, &mut server).await;
        let remote = StreamKey {
            peer: PeerId(1),
            stream_id: key.stream_id,
        };
        server.poll_events(8);
        assert_eq!(server.failure_kind(), None);
        if expected == FailureKind::SlowConsumer {
            for _ in 0..32 {
                client
                    .inject_packet(
                        PeerId(0),
                        wire::encode(key.stream_id, &Frame::WindowUpdate { consumed: 0 }).unwrap(),
                    )
                    .await
                    .unwrap();
            }
        } else {
            let _ = server
                .inject_subject("skvoz.static.fixture.routes.1.g1.1.g0".into(), vec![0])
                .await;
        }
        let until = Instant::now() + Duration::from_secs(3);
        while server.failure_kind().is_none() {
            assert!(
                Instant::now() < until,
                "static callback not observed: {expected:?}"
            );
            tokio::task::yield_now().await;
        }
        assert_eq!(server.failure_kind(), Some(expected));
        let events = server.poll_events(8);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].key, remote);
        assert!(matches!(
            events[0].event,
            Event::Closed {
                reason: CloseReason::TransportLost
            }
        ));
        assert!(server.poll_events(8).is_empty());
        assert!(server.send(remote, b"revive").is_err());
        assert!(server.open(PeerId(1), b"revive").is_err());
        let resources = server.resources();
        assert_eq!(resources.pending_send_bytes, 0);
        assert_eq!(resources.buffered_receive_bytes, 0);
        assert_eq!(resources.receive_unconsumed_bytes, 0);
        drop(server);
        drop(client);
    }
}
