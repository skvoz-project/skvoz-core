use super::*;
use crate::Config;
fn profile(server: bool) -> ManagerConfig {
    ManagerConfig {
        stream: Config {
            receive_window: crate::MAX_RECEIVE_WINDOW,
            max_frame: 16384,
            max_pending_frames: 128,
            max_metadata: 512,
            open_timeout_ms: 15000,
        },
        max_peers: if server { 128 } else { 1 },
        max_streams: if server { 2048 } else { 512 },
        max_streams_per_peer: 512,
        receive_budget: if server { 128 << 20 } else { 32 << 20 },
        receive_budget_per_peer: 32 << 20,
        send_budget: 64 << 20,
        send_budget_per_peer: 2 << 20,
    }
}
fn pair() -> (Manager, Manager) {
    let mut a = Manager::new(profile(false)).unwrap();
    let mut b = Manager::new(profile(true)).unwrap();
    a.register_peer(PeerId(1), false).unwrap();
    b.register_peer(PeerId(1), true).unwrap();
    (a, b)
}
fn forward(a: &mut Manager, b: &mut Manager, now: u64) {
    for f in a.poll_frames(256) {
        b.receive(f.key, &f.frame, now).unwrap();
    }
}
fn established(a: &mut Manager, b: &mut Manager) -> StreamKey {
    let key = a.open(PeerId(1), b"", 0).unwrap();
    forward(a, b, 0);
    for e in b.poll_events(256) {
        if matches!(e.event, Event::IncomingOpen { .. }) {
            b.accept(e.key, b"").unwrap();
        }
    }
    forward(b, a, 0);
    a.poll_events(256);
    key
}
#[test]
fn own_consumption_grows_a_sole_flow_and_does_not_grow_an_idle_neighbor() {
    let (mut a, mut b) = pair();
    let bulk = established(&mut a, &mut b);
    let neighbor = established(&mut a, &mut b);
    let mut received = 0u64;
    for now in 1..160 {
        for _ in 0..128 {
            if a.send(bulk, &[11; 16384]).unwrap() == SendOutcome::WouldBlock {
                break;
            }
        }
        forward(&mut a, &mut b, now);
        for event in b.poll_events(256) {
            if let Event::Data { offset, bytes } = event.event {
                assert_eq!(event.key, bulk);
                assert_eq!(offset, received);
                assert!(bytes.iter().all(|byte| *byte == 11));
                received += bytes.len() as u64;
                b.consume_through(bulk, received).unwrap();
            }
        }
        forward(&mut b, &mut a, now);
        a.poll_events(256);
    }
    assert!(received > 8 << 20);
    assert!(b.streams[&bulk].stream.receive_window() > 1 << 20);
    assert_eq!(b.streams[&neighbor].stream.receive_window(), 65536);
    assert!(b.pressure_deadlines.is_empty());
    assert!(b.receive_promises.bytes <= b.config.receive_budget as u64);
}
fn queued_growth_sample() -> (Manager, StreamKey) {
    let (mut sender, mut receiver) = pair();
    let key = established(&mut sender, &mut receiver);
    for _ in 0..3 {
        assert_eq!(
            sender.send(key, &[7; 16384]),
            Ok(SendOutcome::Accepted(16384))
        );
    }
    forward(&mut sender, &mut receiver, 1);
    receiver.poll_events(256);
    receiver
        .receive(
            StreamKey {
                peer: key.peer,
                stream_id: 0,
            },
            &Frame::PeerRequest {
                bytes: 1048576,
                records: 64,
                probe: 0,
                requester_stream_id: key.stream_id,
                blocked: 0,
            },
            1,
        )
        .unwrap();
    receiver.peers.get_mut(&key.peer).unwrap().rtt_ms = 2;
    receiver.tick(1000).unwrap();
    (receiver, key)
}
#[test]
fn consumption_growth_excludes_wait_before_own_processing_starts() {
    let (mut receiver, key) = queued_growth_sample();
    receiver.consume_through(key, 16384).unwrap();
    receiver.consume_through(key, 32768).unwrap();
    assert_eq!(receiver.streams[&key].stream.receive_window(), 262144);
    assert!(receiver.receive_promises.bytes <= receiver.config.receive_budget as u64);
}
#[test]
fn consumption_growth_recovers_when_shared_owner_drains_the_stream() {
    let (mut receiver, key) = queued_growth_sample();
    receiver.consume_through(key, 16384).unwrap();
    receiver.tick(1100).unwrap();
    receiver.consume_through(key, 49152).unwrap();
    assert_eq!(receiver.streams[&key].stream.receive_window(), 262144);
    assert_eq!(
        receiver.streams[&key].received,
        receiver.streams[&key].consumed
    );
}
#[test]
fn consumption_growth_still_rejects_slow_backlogged_processing() {
    let (mut receiver, key) = queued_growth_sample();
    receiver.consume_through(key, 16384).unwrap();
    receiver.tick(1100).unwrap();
    receiver.consume_through(key, 32768).unwrap();
    assert_eq!(receiver.streams[&key].stream.receive_window(), 65536);
}
#[test]
fn retained_pending_and_dispatched_records_keep_same_peer_headroom() {
    let (mut a, mut b) = pair();
    let stalled = established(&mut a, &mut b);
    let healthy = established(&mut a, &mut b);
    for _ in 0..56 {
        assert_eq!(a.send(stalled, &[1]), Ok(SendOutcome::Accepted(1)));
    }
    assert_eq!(a.send(stalled, &[1]), Ok(SendOutcome::WouldBlock));
    assert_eq!(a.streams[&stalled].stream.retained_send_records(), 56);
    forward(&mut a, &mut b, 1);
    assert_eq!(a.pending_records, 0);
    assert_eq!(a.send(stalled, &[1]), Ok(SendOutcome::WouldBlock));
    assert_eq!(a.send(healthy, b"healthy"), Ok(SendOutcome::Accepted(7)));
    forward(&mut a, &mut b, 1);
    let mut exact = Vec::new();
    for event in b.poll_events(256) {
        if let Event::Data { offset, bytes } = event.event
            && event.key == healthy
        {
            assert_eq!(offset, 0);
            exact.extend_from_slice(&bytes);
            b.consume_through(healthy, bytes.len() as u64).unwrap();
        }
    }
    assert_eq!(exact, b"healthy");
    assert_eq!(b.peers[&PeerId(1)].rx.outstanding().records, 56);
}
#[test]
fn byte_headroom_progresses_another_stream_and_partial_ack_wakes_own_gate() {
    let (mut a, mut b) = pair();
    let stalled = established(&mut a, &mut b);
    let healthy = established(&mut a, &mut b);
    let mut accepted = 0;
    while let SendOutcome::Accepted(n) = a.send(stalled, &[3; 16384]).unwrap() {
        accepted += n;
    }
    assert_eq!(accepted, 57344);
    assert_eq!(
        a.snapshot(stalled).unwrap().send_unacknowledged_bytes,
        57344
    );
    forward(&mut a, &mut b, 1);
    assert_eq!(a.send(stalled, &[3]), Ok(SendOutcome::WouldBlock));
    assert_eq!(
        a.send(healthy, &[9; 16384]),
        Ok(SendOutcome::Accepted(8192))
    );
    assert_eq!(a.send(healthy, &[9]), Ok(SendOutcome::WouldBlock));
    forward(&mut a, &mut b, 1);
    let mut host_owned = b.poll_events(256);
    assert_eq!(b.peers[&PeerId(1)].rx.outstanding().bytes, 65536);
    assert_eq!(b.peers[&PeerId(1)].rx.outstanding().records, 5);
    // Deliver aggregate ACK before the partial stream ACK deliberately.
    // Keep this fixture's peer allowance unchanged to isolate the own gate.
    let peer = b.peers.get_mut(&PeerId(1)).unwrap();
    peer.requested = peer.target;
    b.consume_through(stalled, 1).unwrap();
    let mut frames = b.poll_frames(256);
    let aggregate = frames
        .iter()
        .position(|f| matches!(f.frame, Frame::PeerGrant { .. }))
        .unwrap();
    let frame = frames.remove(aggregate);
    a.receive(frame.key, &frame.frame, 2).unwrap();
    a.poll_events(256);
    assert_eq!(a.send(stalled, &[3]), Ok(SendOutcome::WouldBlock));
    assert!(a.budget_waiters.contains(&stalled));
    for frame in frames {
        a.receive(frame.key, &frame.frame, 2).unwrap();
    }
    assert!(
        a.poll_events(256)
            .iter()
            .any(|e| e.key == stalled && e.event == Event::Writable)
    );
    assert_eq!(a.send(stalled, &[3]), Ok(SendOutcome::Accepted(1)));
    let exact = host_owned
        .iter()
        .find_map(|e| match &e.event {
            Event::Data { bytes, .. } if e.key == healthy => Some(bytes.as_ref()),
            _ => None,
        })
        .unwrap();
    assert_eq!(exact, &[9; 8192]);
    b.consume_through(healthy, 8192).unwrap();
    b.tick(1002).unwrap();
    assert_eq!(b.snapshot(stalled).unwrap().state, State::Open);
    assert_eq!(b.snapshot(healthy).unwrap().state, State::Open);
    host_owned.clear();
    b.close(stalled, CloseReason::Cancelled).unwrap();
    b.close(healthy, CloseReason::Cancelled).unwrap();
    assert_eq!(b.resources().receive_unconsumed_bytes, 0);
}
fn pressure_request(requester: StreamKey, blocked: u8) -> Frame {
    Frame::PeerRequest {
        bytes: 1 << 20,
        records: 128,
        probe: 0,
        requester_stream_id: requester.stream_id,
        blocked,
    }
}
#[test]
fn byte_only_pressure_retires_largest_unread_flow_and_preserves_other_fin() {
    let (mut a, mut b) = pair();
    let largest = established(&mut a, &mut b);
    let smaller = established(&mut a, &mut b);
    let requester = established(&mut a, &mut b);
    for offset in [0, 16384, 32768] {
        b.receive(
            largest,
            &Frame::Data {
                offset,
                bytes: vec![3; 16384].into_boxed_slice(),
            },
            1,
        )
        .unwrap();
    }
    b.receive(
        smaller,
        &Frame::Data {
            offset: 0,
            bytes: vec![4; 16384].into_boxed_slice(),
        },
        1,
    )
    .unwrap();
    b.receive(
        smaller,
        &Frame::Fin {
            final_offset: 16384,
        },
        1,
    )
    .unwrap();
    let mut retained = b.poll_events(256);
    assert_eq!(
        b.peers[&largest.peer].rx.outstanding(),
        Amount {
            bytes: 65536,
            records: 4
        }
    );
    b.receive(
        StreamKey {
            peer: largest.peer,
            stream_id: 0,
        },
        &pressure_request(requester, 1),
        100,
    )
    .unwrap();
    b.tick(1099).unwrap();
    assert_eq!(b.snapshot(largest).unwrap().state, State::Open);
    b.tick(1100).unwrap();
    assert_eq!(b.snapshot(largest).unwrap().state, State::Closed);
    assert_eq!(b.snapshot(smaller).unwrap().state, State::HalfClosedRemote);
    assert_eq!(b.snapshot(requester).unwrap().state, State::Open);
    assert_eq!(
        b.peers[&largest.peer].rx.outstanding(),
        Amount {
            bytes: 16384,
            records: 1
        }
    );
    retained.retain(|event| event.key != largest);
    b.receive(
        requester,
        &Frame::Data {
            offset: 0,
            bytes: Box::new(*b"exact"),
        },
        1100,
    )
    .unwrap();
    let exact = b
        .poll_events(256)
        .into_iter()
        .find_map(|event| match event.event {
            Event::Data { offset, bytes } if event.key == requester => {
                assert_eq!(offset, 0);
                Some(bytes)
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(&*exact, b"exact");
    b.consume_through(requester, 5).unwrap();
    b.consume_through(smaller, 16384).unwrap();
    assert_eq!(b.resources().receive_unconsumed_bytes, 0);
    assert!(b.unconsumed.is_empty());
    assert!(b.receive_promises.bytes <= b.config.receive_budget as u64);
}
#[test]
fn transport_credit_keeps_open_order_without_blocking_smaller_live_frames() {
    let mut config = profile(false);
    config.max_peers = 2;
    let mut sender = Manager::new(config).unwrap();
    let mut receiver = Manager::new(profile(true)).unwrap();
    sender.register_peer(PeerId(1), false).unwrap();
    receiver.register_peer(PeerId(1), true).unwrap();
    let live = established(&mut sender, &mut receiver);
    let keys: Vec<_> = [400, 1, 100]
        .into_iter()
        .map(|size| sender.open(PeerId(1), &vec![7; size], 0).unwrap())
        .collect();
    let cancelled = sender.open(PeerId(1), b"cancel", 0).unwrap();
    sender.close(cancelled, CloseReason::Cancelled).unwrap();
    sender.register_peer(PeerId(2), false).unwrap();
    let other = sender.open(PeerId(2), b"other", 0).unwrap();
    sender.close(other, CloseReason::Cancelled).unwrap();
    assert_eq!(
        sender.send(live, b"tail").unwrap(),
        SendOutcome::Accepted(4)
    );
    sender.finish(live).unwrap();

    let mut attempts = 0;
    let mut emitted = Vec::new();
    for _ in 0..4 {
        emitted.extend(sender.poll_frames_with_budget(256, |_, size, _| {
            attempts += 1;
            size <= 64
        }));
    }
    assert!(attempts <= 4 * (256 + 6 + 2));
    assert!(
        !emitted
            .iter()
            .any(|f| matches!(f.frame, Frame::Open { .. }))
    );
    for key in [cancelled, other] {
        assert!(
            emitted
                .iter()
                .any(|f| f.key == key && matches!(f.frame, Frame::Close { .. }))
        );
    }
    assert!(
        emitted
            .iter()
            .any(|f| f.key.peer == PeerId(2) && matches!(f.frame, Frame::PeerGrant { .. }))
    );
    assert!(
        emitted.iter().any(|f| f.key == live
            && matches!(&f.frame, Frame::Data { bytes, .. } if &**bytes == b"tail"))
    );
    assert!(
        emitted
            .iter()
            .any(|f| f.key == live && matches!(f.frame, Frame::Fin { final_offset: 4 }))
    );
    // A smaller, barred OPEN must not keep a recipient grant open when
    // its remainder cannot carry the oldest eligible OPEN.
    assert_eq!(
        sender.next_transport_weight(PeerId(1), 24),
        Some(24 + 28 + 400)
    );
    for frame in emitted.iter().filter(|f| f.key.peer == PeerId(1)) {
        receiver.receive(frame.key, &frame.frame, 0).unwrap();
    }
    let mut published = Vec::new();
    for _ in 0..3 {
        let frames = sender.poll_frames_with_budget(1, |_, _, _| true);
        assert_eq!(frames.len(), 1);
        assert!(matches!(frames[0].frame, Frame::Open { .. }));
        published.push(frames[0].key);
        receiver
            .receive(frames[0].key, &frames[0].frame, 0)
            .unwrap();
    }
    assert_eq!(published, keys);
    let incoming: Vec<_> = receiver
        .poll_events(256)
        .into_iter()
        .filter(|event| matches!(event.event, Event::IncomingOpen { .. }))
        .map(|event| event.key)
        .collect();
    assert_eq!(incoming, keys);
    for key in &incoming {
        receiver.accept(*key, b"").unwrap();
    }
    forward(&mut receiver, &mut sender, 0);
    assert_eq!(
        sender
            .poll_events(256)
            .into_iter()
            .filter(|event| matches!(event.event, Event::Opened { .. }))
            .count(),
        keys.len()
    );
}

#[test]
fn delivery_frame_gate_preserves_pending_data_other_peer_cancel_and_freeze_order() {
    let mut sender = Manager::new(profile(true)).unwrap();
    let mut keys = Vec::new();
    for peer in [PeerId(1), PeerId(2)] {
        sender.register_peer(peer, false).unwrap();
        let key = sender.open(peer, b"", 0).unwrap();
        sender.poll_frames(256);
        sender
            .receive(
                key,
                &Frame::Accept {
                    receive_window: 32 << 20,
                    max_frame: 16384,
                    metadata: Box::new([]),
                },
                0,
            )
            .unwrap();
        sender
            .receive(
                StreamKey { peer, stream_id: 0 },
                &Frame::PeerGrant {
                    epoch: 0,
                    limit_bytes: 65536,
                    limit_records: 64,
                    consumed_bytes: 0,
                    consumed_records: 0,
                    probe: 0,
                },
                0,
            )
            .unwrap();
        sender.poll_events(256);
        keys.push(key);
    }
    sender.poll_frames(256);
    assert_eq!(
        sender.send(keys[0], &[1; 10000]).unwrap(),
        SendOutcome::Accepted(10000)
    );
    let before = sender.snapshot(keys[0]).unwrap();
    sender
        .receive(
            StreamKey {
                peer: keys[0].peer,
                stream_id: 0,
            },
            &Frame::PeerFreeze { epoch: 1 },
            1,
        )
        .unwrap();
    sender.close(keys[1], CloseReason::Cancelled).unwrap();
    let mut attempts = 0;
    let terminal = sender.poll_frames_with_budget(1, |_, _, data| {
        attempts += 1;
        !data
    });
    assert_eq!(terminal.len(), 1);
    assert_eq!(terminal[0].key, keys[1]);
    assert!(matches!(
        terminal[0].frame,
        Frame::Close {
            reason: CloseReason::Cancelled
        }
    ));
    assert!(
        sender
            .poll_frames_with_budget(8, |_, _, data| {
                attempts += 1;
                !data
            })
            .is_empty()
    );
    assert!(attempts <= 1 + 2 + 2 + 8 + 2 + 2);
    assert_eq!(
        sender.snapshot(keys[0]).unwrap().pending_send_bytes,
        before.pending_send_bytes
    );
    assert_eq!(sender.peers[&keys[0].peer].tx.dispatched, Amount::default());
    assert!(!sender.peers[&keys[0].peer].tx.frozen);
    let frames = sender.poll_frames(8);
    assert!(matches!(frames[0].frame, Frame::Data { .. }));
    assert!(matches!(
        frames[1].frame,
        Frame::PeerFrozen {
            bytes: 10000,
            records: 1,
            ..
        }
    ));
    assert_eq!(sender.resources().pending_send_bytes, 0);
}
#[test]
fn framed_demand_survives_data_scheduling_yields_without_a_rejection() {
    let (mut sender, mut recipient) = pair();
    let key = established(&mut sender, &mut recipient);
    sender.poll_frames(256);
    for _ in 0..3 {
        assert_eq!(
            sender.send(key, &[1; 16384]).unwrap(),
            SendOutcome::Accepted(16384)
        );
    }
    let before = sender.transport_demand(key.peer, 540, 3 << 20);
    let first = sender.poll_frames(1).pop().unwrap();
    assert!(matches!(first.frame, Frame::Data { .. }));
    assert!(sender.peers[&key.peer].reject_next);
    assert!(sender.peers[&key.peer].rejection.is_none());
    assert!(sender.next_transport_weight(key.peer, 540).is_some());
    let after = sender.transport_demand(key.peer, 540, 3 << 20);
    assert!(after > 0 && after < before);
    let next = sender.poll_frames(1).pop().unwrap();
    assert!(matches!(next.frame, Frame::Data { .. }));
}
#[test]
fn requester_remote_fin_clears_pressure_without_retiring_unread_neighbor() {
    let (mut a, mut b) = pair();
    let stalled = established(&mut a, &mut b);
    let requester = established(&mut a, &mut b);
    for offset in [0, 16384, 32768, 49152] {
        b.receive(
            stalled,
            &Frame::Data {
                offset,
                bytes: vec![3; 16384].into_boxed_slice(),
            },
            1,
        )
        .unwrap();
    }
    let retained = b.poll_events(256);
    let outstanding = b.peers[&stalled.peer].rx.outstanding();
    b.receive(
        StreamKey {
            peer: stalled.peer,
            stream_id: 0,
        },
        &pressure_request(requester, 1),
        100,
    )
    .unwrap();
    assert!(b.peers[&stalled.peer].pressure.is_some());
    b.receive(requester, &Frame::Fin { final_offset: 0 }, 101)
        .unwrap();
    assert_eq!(
        b.snapshot(requester).unwrap().state,
        State::HalfClosedRemote
    );
    b.tick(1100).unwrap();
    assert_eq!(b.snapshot(stalled).unwrap().state, State::Open);
    assert!(b.peers[&stalled.peer].pressure.is_none());
    assert!(b.pressure_deadlines.is_empty());
    assert_eq!(b.peers[&stalled.peer].rx.outstanding(), outstanding);
    b.close(stalled, CloseReason::Cancelled).unwrap();
    b.close(requester, CloseReason::Cancelled).unwrap();
    drop(retained);
    assert_eq!(b.resources().receive_unconsumed_bytes, 0);
}
#[test]
fn multiple_unread_streams_and_changing_requesters_have_a_finite_pressure_deadline() {
    let (mut a, mut b) = pair();
    let first = established(&mut a, &mut b);
    let second = established(&mut a, &mut b);
    let healthy = established(&mut a, &mut b);
    for _ in 0..56 {
        a.send(first, &[1]).unwrap();
    }
    for _ in 0..8 {
        a.send(second, &[2]).unwrap();
    }
    forward(&mut a, &mut b, 1);
    let mut host_owned = b.poll_events(256);
    assert_eq!(b.peers[&PeerId(1)].rx.outstanding().records, 64);
    assert_eq!(
        a.send(healthy, b"after-pressure"),
        Ok(SendOutcome::WouldBlock)
    );
    forward(&mut a, &mut b, 100);
    let control = StreamKey {
        peer: PeerId(1),
        stream_id: 0,
    };
    let deadline = b.peers[&PeerId(1)].pressure.unwrap().deadline;
    for now in 101..200 {
        b.receive(
            control,
            &pressure_request(
                if now % 2 == 0 { healthy } else { second },
                if now % 2 == 0 { 2 } else { 1 },
            ),
            now,
        )
        .unwrap();
        assert_eq!(b.peers[&PeerId(1)].pressure.unwrap().deadline, deadline);
    }
    // Keep the real healthy requester excluded from victim selection.
    b.receive(control, &pressure_request(healthy, 2), 200)
        .unwrap();
    assert_eq!(b.peers[&PeerId(1)].pressure.unwrap().blocked, 3);
    b.tick(deadline - 1).unwrap();
    assert_eq!(b.snapshot(first).unwrap().state, State::Open);
    b.tick(deadline).unwrap();
    assert_eq!(b.snapshot(first).unwrap().state, State::Closed);
    assert_eq!(b.snapshot(second).unwrap().state, State::Open);
    assert_eq!(b.snapshot(healthy).unwrap().state, State::Open);
    // The owning host releases cancelled buffers before receiving new ones.
    host_owned.retain(|event| event.key != first);
    forward(&mut b, &mut a, deadline);
    a.poll_events(256);
    assert_eq!(
        a.send(healthy, b"after-pressure"),
        Ok(SendOutcome::Accepted(14))
    );
    forward(&mut a, &mut b, deadline);
    let mut exact = Vec::new();
    for event in b.poll_events(256) {
        if let Event::Data { offset, bytes } = event.event
            && event.key == healthy
        {
            assert_eq!(offset, 0);
            exact.extend_from_slice(&bytes);
            b.consume_through(healthy, bytes.len() as u64).unwrap();
        }
    }
    assert_eq!(exact, b"after-pressure");
    assert_eq!(b.peers[&PeerId(1)].rx.outstanding().records, 8);
    assert!(b.receive_promises.bytes <= b.config.receive_budget as u64);
    assert!(b.receive_promises.records <= b.config.receive_record_budget() as u64);
    drop(host_owned);
    b.close(second, CloseReason::Cancelled).unwrap();
    b.close(healthy, CloseReason::Cancelled).unwrap();
    for _ in 0..4 {
        forward(&mut b, &mut a, deadline);
        forward(&mut a, &mut b, deadline);
        b.poll_events(256);
        a.poll_events(256);
    }
    assert_eq!(b.resources().receive_unconsumed_bytes, 0);
    assert_eq!(b.resources().streams, 0);
    assert!(b.unconsumed.is_empty());
}
#[test]
fn own_request_cannot_retire_a_sole_paused_receiver() {
    let (mut a, mut b) = pair();
    let paused = established(&mut a, &mut b);
    // A legal full incoming peer allowance, independently of local send policy.
    for offset in 0..64 {
        b.receive(
            paused,
            &Frame::Data {
                offset,
                bytes: Box::new([7]),
            },
            1,
        )
        .unwrap();
    }
    b.receive(
        StreamKey {
            peer: paused.peer,
            stream_id: 0,
        },
        &pressure_request(paused, 2),
        2,
    )
    .unwrap();
    for now in [1002, 5000, 30000, 60000] {
        b.tick(now).unwrap();
    }
    assert_eq!(b.snapshot(paused).unwrap().state, State::Open);
    assert_eq!(b.peers[&paused.peer].rx.outstanding().records, 64);
    assert!(!b.peers[&paused.peer].failed);
}
#[test]
fn fresh_data_on_an_old_idle_stream_does_not_inherit_stall_age() {
    let (mut a, mut b) = pair();
    let fresh = established(&mut a, &mut b);
    let requester = established(&mut a, &mut b);
    let control = StreamKey {
        peer: fresh.peer,
        stream_id: 0,
    };
    b.receive(control, &pressure_request(requester, 2), 50000)
        .unwrap();
    for offset in 0..64 {
        b.receive(
            fresh,
            &Frame::Data {
                offset,
                bytes: Box::new([8]),
            },
            50999,
        )
        .unwrap();
    }
    b.tick(51000).unwrap();
    assert_eq!(b.snapshot(fresh).unwrap().state, State::Open);
    assert_eq!(b.streams[&fresh].last_consumption, 50999);
    assert_eq!(b.streams[&fresh].growth_started, 50999);
    b.tick(51999).unwrap();
    // The rescheduled deadline remains bounded, rather than closing at age1ms.
    b.tick(52000).unwrap();
    assert_eq!(b.snapshot(fresh).unwrap().state, State::Closed);
}
#[test]
fn pressure_request_identity_and_counter_boundaries_are_checked() {
    let (mut a, mut b) = pair();
    let key = established(&mut a, &mut b);
    let control = StreamKey {
        peer: key.peer,
        stream_id: 0,
    };
    let future = StreamKey {
        peer: key.peer,
        stream_id: key.stream_id + 2,
    };
    assert_eq!(
        b.receive(control, &pressure_request(future, 2), 1),
        Err(ManagerError::InvalidOrigin)
    );
    b.close(key, CloseReason::Cancelled).unwrap();
    assert_eq!(b.receive(control, &pressure_request(key, 2), 2), Ok(()));
    assert!(b.pressure_deadlines.is_empty());
    let (mut a, mut b) = pair();
    let key = established(&mut a, &mut b);
    assert_eq!(
        b.receive(control, &pressure_request(key, 2), u64::MAX),
        Err(ManagerError::Stream(Error::InvalidTime))
    );
}
#[test]
fn duplicate_grants_preserve_request_suppression_but_thaw_renews_demand() {
    let (mut a, mut b) = pair();
    let first = established(&mut a, &mut b);
    let second = established(&mut a, &mut b);
    let requester = established(&mut a, &mut b);
    for _ in 0..56 {
        a.send(first, &[1]).unwrap();
    }
    for _ in 0..8 {
        a.send(second, &[2]).unwrap();
    }
    assert_eq!(a.send(requester, &[3]), Ok(SendOutcome::WouldBlock));
    forward(&mut a, &mut b, 1);
    let suppressed = a.peers[&requester.peer].last_request;
    assert!(suppressed.is_some());
    let control = StreamKey {
        peer: requester.peer,
        stream_id: 0,
    };
    a.receive(control, &b.peers[&requester.peer].rx.frame(0), 2)
        .unwrap();
    assert_eq!(a.peers[&requester.peer].last_request, suppressed);
    assert_eq!(a.send(requester, &[3]), Ok(SendOutcome::WouldBlock));
    assert!(
        !a.poll_frames(256)
            .iter()
            .any(|f| matches!(f.frame, Frame::PeerRequest { .. }))
    );
    let freeze = b
        .peers
        .get_mut(&requester.peer)
        .unwrap()
        .rx
        .freeze()
        .unwrap();
    a.receive(control, &freeze, 3).unwrap();
    let frozen = a
        .poll_frames(256)
        .into_iter()
        .find(|f| matches!(f.frame, Frame::PeerFrozen { .. }))
        .unwrap();
    // An old demand can expire while the direction is frozen. No arrived
    // DATA is freed, and the delayed ordered ACK retains the same totals.
    b.tick(1001).unwrap();
    assert!(b.peers[&requester.peer].pressure.is_none());
    b.receive(frozen.key, &frozen.frame, 1002).unwrap();
    a.receive(control, &b.peers[&requester.peer].rx.frame(0), 1002)
        .unwrap();
    assert!(a.peers[&requester.peer].last_request.is_none());
    assert_eq!(a.send(requester, &[3]), Ok(SendOutcome::WouldBlock));
    assert!(a.poll_frames(256).iter().any(|f|matches!(f.frame,
        Frame::PeerRequest{requester_stream_id,blocked:2,..} if requester_stream_id==requester.stream_id)));
    assert_eq!(b.peers[&requester.peer].rx.outstanding().records, 64);
    assert!(b.receive_promises.records <= b.config.receive_record_budget() as u64);
}
#[test]
fn bounded_event_admission_retains_burst_and_progresses_other_streams() {
    let (mut a, mut b) = pair();
    let ip = established(&mut a, &mut b);
    let tcp = established(&mut a, &mut b);
    let terminal = established(&mut a, &mut b);
    // Actual consumption grows the adaptive stream/peer window first.
    let mut prefix = 0;
    for now in 1..24 {
        for _ in 0..128 {
            match a.send(ip, &[7; 16384]).unwrap() {
                SendOutcome::Accepted(n) => prefix += n as u64,
                SendOutcome::WouldBlock => break,
            }
        }
        forward(&mut a, &mut b, now);
        for event in b.poll_events(256) {
            if let Event::Data { offset, bytes } = event.event {
                b.consume_through(event.key, offset + bytes.len() as u64)
                    .unwrap();
            }
        }
        forward(&mut b, &mut a, now);
        a.poll_events(256);
    }
    // A byte-sliced scheduler can leave a bounded suffix after one batch.
    // Finish the warmup before asserting the next payload's exact prefix.
    for now in 24..40 {
        if a.aggregate().pending_send_bytes == 0 {
            break;
        }
        forward(&mut a, &mut b, now);
        for event in b.poll_events(256) {
            if let Event::Data { offset, bytes } = event.event {
                b.consume_through(event.key, offset + bytes.len() as u64)
                    .unwrap();
            }
        }
        forward(&mut b, &mut a, now);
        a.poll_events(256);
    }
    assert_eq!(a.aggregate().pending_send_bytes, 0);
    for _ in 0..8 {
        assert_eq!(a.send(ip, &[9; 16384]), Ok(SendOutcome::Accepted(16384)));
    }
    assert_eq!(a.send(tcp, b"healthy"), Ok(SendOutcome::Accepted(7)));
    a.close(terminal, CloseReason::Cancelled).unwrap();
    forward(&mut a, &mut b, 40);
    let mut room = 65536;
    let events = b.poll_events_with_data_budget(256, |key, bytes| {
        if key != ip {
            return true;
        }
        if bytes > room {
            return false;
        }
        room -= bytes;
        true
    });
    assert_eq!(room, 0);
    assert!(
        events.iter().any(|e| e.key == tcp
            && matches!(&e.event, Event::Data { bytes, .. } if &**bytes == b"healthy"))
    );
    assert!(
        events
            .iter()
            .any(|e| e.key == terminal && matches!(e.event, Event::Closed { .. }))
    );
    let mut received = 0;
    for event in events {
        if event.key == ip
            && let Event::Data { offset, bytes } = event.event
        {
            assert_eq!(offset, prefix + received);
            assert!(bytes.iter().all(|byte| *byte == 9));
            received += bytes.len() as u64;
            b.consume_through(ip, offset + bytes.len() as u64).unwrap();
        }
    }
    assert_eq!(received, 65536);
    let mut attempts = 0;
    assert!(
        b.poll_events_with_data_budget(8, |_, _| {
            attempts += 1;
            false
        })
        .is_empty()
    );
    assert!(attempts > 0 && attempts <= 8 + b.resources().streams);
    assert_eq!(b.snapshot(ip).unwrap().buffered_receive_bytes, 65536);
    for event in b.poll_events_with_data_budget(256, |_, _| true) {
        if event.key == ip
            && let Event::Data { offset, bytes } = event.event
        {
            assert_eq!(offset, prefix + received);
            assert!(bytes.iter().all(|byte| *byte == 9));
            received += bytes.len() as u64;
            b.consume_through(ip, offset + bytes.len() as u64).unwrap();
        }
    }
    assert_eq!(received, 131072);
}
#[test]
fn smaller_shared_pool_leaves_active_growth_and_backing_for_future_peers() {
    let mut config = profile(true);
    config.stream.receive_window = 1 << 20;
    config.receive_budget = 8 << 20;
    config.receive_budget_per_peer = 1 << 20;
    let mut client_config = config;
    client_config.max_peers = 1;
    let mut a = Manager::new(client_config).unwrap();
    let mut b = Manager::new(config).unwrap();
    a.register_peer(PeerId(1), false).unwrap();
    b.register_peer(PeerId(1), true).unwrap();
    assert_eq!(b.minimum_credit().bytes, 8192);
    let key = a.open(PeerId(1), b"", 0).unwrap();
    forward(&mut a, &mut b, 0);
    for event in b.poll_events(256) {
        if matches!(event.event, Event::IncomingOpen { .. }) {
            b.accept(event.key, b"").unwrap();
        }
    }
    forward(&mut b, &mut a, 0);
    a.poll_events(256);
    let mut accepted = 0u64;
    let mut received = 0u64;
    for now in 1..8 {
        b.receive(
            StreamKey {
                peer: PeerId(1),
                stream_id: 0,
            },
            &Frame::PeerRequest {
                bytes: 1 << 20,
                records: 128,
                probe: 0,
                requester_stream_id: 2,
                blocked: 0,
            },
            now,
        )
        .unwrap();
        let SendOutcome::Accepted(count) = a.send(key, &[0; 8192]).unwrap() else {
            panic!("consuming stream made no progress");
        };
        assert!((1..=8192).contains(&count));
        accepted += count as u64;
        forward(&mut a, &mut b, now);
        for event in b.poll_events(256) {
            if let Event::Data { offset, bytes } = event.event {
                assert_eq!(event.key, key);
                assert_eq!(offset, received);
                assert!(bytes.iter().all(|byte| *byte == 0));
                received += bytes.len() as u64;
                b.consume_through(event.key, offset + bytes.len() as u64)
                    .unwrap();
            }
        }
        forward(&mut b, &mut a, now);
        a.poll_events(256);
    }
    assert_eq!(received, accepted);
    assert_eq!(b.peers[&PeerId(1)].target.bytes, 1 << 20);
    b.register_peer(PeerId(2), true).unwrap();
    assert_eq!(b.peers[&PeerId(2)].reserved_receive.bytes, 8192);
    assert_eq!(b.peers[&PeerId(2)].reserved_receive.records, 64);
    assert!(b.receive_promises.bytes <= config.receive_budget as u64);
    assert!(b.receive_promises.records <= config.receive_record_budget() as u64);
}
#[test]
fn idle_count_does_not_reserve_maximum_windows_and_bulk_grows_automatically() {
    let (mut a, mut b) = pair();
    let mut keys = Vec::new();
    for _ in 0..512 {
        keys.push(a.open(PeerId(1), b"", 0).unwrap());
    }
    assert_eq!(a.open(PeerId(1), b"", 0), Err(ManagerError::Admission));
    assert_eq!(a.resources().reserved_receive_bytes, 65536);
    assert_eq!(a.resources().receive_capacity_bytes, 0);
    for _ in 0..8 {
        forward(&mut a, &mut b, 0);
        for e in b.poll_events(256) {
            if matches!(e.event, Event::IncomingOpen { .. }) {
                b.accept(e.key, b"").unwrap();
            }
        }
        forward(&mut b, &mut a, 0);
        a.poll_events(256);
    }
    assert_eq!(b.resources().streams, 512);
    let key = keys[0];
    let block = [37; 16384];
    let mut sent = 0;
    let mut received = 0;
    for now in 1..1000 {
        for _ in 0..32 {
            if sent >= 8 << 20 {
                break;
            }
            match a
                .send(key, &block[..block.len().min((8 << 20) - sent)])
                .unwrap()
            {
                SendOutcome::Accepted(n) => sent += n,
                SendOutcome::WouldBlock => break,
            }
        }
        forward(&mut a, &mut b, now);
        for e in b.poll_events(256) {
            if let Event::Data { offset, bytes } = e.event {
                assert_eq!(offset as usize, received);
                assert!(bytes.iter().all(|b| *b == 37));
                received += bytes.len();
                b.consume_through(e.key, offset + bytes.len() as u64)
                    .unwrap();
            }
        }
        forward(&mut b, &mut a, now);
        a.poll_events(256);
        assert!(b.receive_promises.bytes <= b.config.receive_budget as u64);
        assert!(b.receive_promises.records <= b.config.receive_record_budget() as u64);
        if received == 8 << 20 {
            break;
        }
    }
    assert_eq!(received, 8 << 20);
    assert_eq!(sent, received);
    assert!(b.peers[&PeerId(1)].target.bytes >= 1 << 20);
    assert_eq!(b.resources().buffered_receive_bytes, 0);
    assert_eq!(b.resources().receive_capacity_bytes, 0);
}
#[test]
fn duplex_consumption_does_not_add_already_queued_send_bytes_again() {
    let (mut a, mut b) = pair();
    let key = established(&mut a, &mut b);
    assert_eq!(a.send(key, b"outbound"), Ok(SendOutcome::Accepted(8)));
    assert_eq!(b.send(key, b"inbound"), Ok(SendOutcome::Accepted(7)));
    forward(&mut b, &mut a, 1);
    let data = a
        .poll_events(256)
        .into_iter()
        .find_map(|e| match e.event {
            Event::Data { offset, bytes } => Some((offset, bytes)),
            _ => None,
        })
        .unwrap();
    a.consume_through(key, data.0 + data.1.len() as u64)
        .unwrap();
    assert_eq!(a.resources().pending_send_bytes, 8);
    assert_eq!(a.peers[&PeerId(1)].tx.pending, Amount::data(8));
    forward(&mut a, &mut b, 1);
    assert_eq!(a.resources().pending_send_bytes, 0);
    assert_eq!(a.pending_records, 0);
}
#[test]
fn late_cancelled_data_is_accounted_without_recycling_unused_permission() {
    let (mut a, mut b) = pair();
    let key = established(&mut a, &mut b);
    a.send(key, b"late").unwrap();
    let data = a
        .poll_frames(256)
        .into_iter()
        .find(|f| matches!(f.frame, Frame::Data { .. }))
        .unwrap();
    let promised = b.receive_promises;
    b.close(key, CloseReason::Cancelled).unwrap();
    b.poll_events(256);
    b.poll_frames(256);
    assert_eq!(b.resources().streams, 0);
    assert_eq!(b.receive_promises, promised);
    b.receive(data.key, &data.frame, 1).unwrap();
    let p = &b.peers[&PeerId(1)];
    assert_eq!(p.rx.received, Amount::data(4));
    assert_eq!(p.rx.consumed, Amount::data(4));
    assert_eq!(b.receive_promises, promised);
    assert_eq!(b.resources().receive_capacity_bytes, 0);
}
#[test]
fn all_registered_idle_peers_leave_record_and_byte_headroom_for_bulk() {
    let mut b = Manager::new(profile(true)).unwrap();
    for id in 1..=128 {
        b.register_peer(PeerId(id), true).unwrap();
    }
    assert_eq!(b.receive_promises.bytes, 8 << 20);
    assert_eq!(b.receive_promises.records, 8192);
    assert!(b.config.receive_record_budget() >= 16384);
    let mut a = Manager::new(profile(false)).unwrap();
    a.register_peer(PeerId(1), false).unwrap();
    let key = a.open(PeerId(1), b"", 0).unwrap();
    forward(&mut a, &mut b, 0);
    b.accept(key, b"").unwrap();
    for f in b.poll_frames(256) {
        if f.key.peer == PeerId(1) {
            a.receive(f.key, &f.frame, 0).unwrap();
        }
    }
    for now in 1..12 {
        b.receive(
            StreamKey {
                peer: PeerId(1),
                stream_id: 0,
            },
            &Frame::PeerRequest {
                bytes: 32 << 20,
                records: 2112,
                probe: 0,
                requester_stream_id: 2,
                blocked: 0,
            },
            now,
        )
        .unwrap();
        assert_eq!(a.send(key, &[0; 16384]), Ok(SendOutcome::Accepted(16384)));
        forward(&mut a, &mut b, now);
        for e in b.poll_events(256) {
            if let Event::Data { offset, bytes } = e.event {
                b.consume_through(e.key, offset + bytes.len() as u64)
                    .unwrap();
            }
        }
        for f in b.poll_frames(256) {
            if f.key.peer == PeerId(1) {
                a.receive(f.key, &f.frame, now).unwrap();
            }
        }
        a.poll_events(256);
    }
    assert_eq!(b.peers[&PeerId(1)].target.bytes, 32 << 20);
    assert!(b.peers[&PeerId(1)].target.records > 64);
    assert!(b.receive_promises.bytes <= 128 << 20);
    assert!(b.receive_promises.records <= b.config.receive_record_budget() as u64);
}

#[test]
fn output_readiness_holds_grants_data_and_frozen_ack_without_extraction() {
    let (mut a, mut b) = pair();
    a.set_peer_output_enabled(PeerId(1), false).unwrap();
    let key = a.open(PeerId(1), b"", 0).unwrap();
    assert!(a.poll_frames(256).is_empty());
    assert_eq!(a.receive_promises.bytes, 65536);
    a.set_peer_output_enabled(PeerId(1), true).unwrap();
    forward(&mut a, &mut b, 0);
    b.accept(key, b"").unwrap();
    forward(&mut b, &mut a, 0);
    a.poll_events(256);
    a.set_peer_output_enabled(PeerId(1), false).unwrap();
    a.receive(
        StreamKey {
            peer: PeerId(1),
            stream_id: 0,
        },
        &Frame::PeerFreeze { epoch: 1 },
        1,
    )
    .unwrap();
    assert!(a.poll_frames(256).is_empty());
    a.set_peer_output_enabled(PeerId(1), true).unwrap();
    assert!(
        a.poll_frames(256)
            .iter()
            .any(|f| matches!(f.frame, Frame::PeerFrozen { epoch: 1, .. }))
    );
}

#[test]
fn dispatched_send_metadata_is_bounded_with_withheld_ack_and_other_peer_progress() {
    let mut config = profile(true);
    config.receive_budget = 4 << 20;
    config.max_peers = 2;
    for late_join in [false, true] {
        let mut a = Manager::new(config).unwrap();
        a.register_peer(PeerId(1), false).unwrap();
        if !late_join {
            a.register_peer(PeerId(2), false).unwrap();
        }
        let mut keys = Vec::new();
        let setup = |a: &mut Manager, peer| {
            let key = a.open(peer, b"", 0).unwrap();
            a.poll_frames(256);
            a.receive(
                key,
                &Frame::Accept {
                    receive_window: 32 << 20,
                    max_frame: 16384,
                    metadata: Box::new([]),
                },
                0,
            )
            .unwrap();
            a.receive(
                StreamKey { peer, stream_id: 0 },
                &Frame::PeerGrant {
                    epoch: 0,
                    consumed_bytes: 0,
                    limit_bytes: 32 << 20,
                    consumed_records: 0,
                    limit_records: 65536,
                    probe: 0,
                },
                0,
            )
            .unwrap();
            a.poll_events(256);
            key
        };
        keys.push(setup(&mut a, PeerId(1)));
        if !late_join {
            keys.push(setup(&mut a, PeerId(2)));
        }
        let mut dispatched = 0;
        loop {
            match a.send(keys[0], b"x").unwrap() {
                SendOutcome::Accepted(1) => {
                    dispatched += 1;
                    a.poll_frames(256);
                }
                SendOutcome::WouldBlock => break,
                other => panic!("unexpected send {other:?}"),
            }
        }
        assert_eq!(a.pending_records, 0);
        assert_eq!(a.retained_send_records, dispatched);
        assert!(dispatched <= a.config.send_record_budget() - 64);
        if late_join {
            a.register_peer(PeerId(2), false).unwrap();
            keys.push(setup(&mut a, PeerId(2)));
        }
        assert_eq!(a.send(keys[1], b"healthy"), Ok(SendOutcome::Accepted(7)));
        a.poll_frames(256);
        assert!(a.retained_send_records <= a.config.send_record_budget());
        a.close(keys[0], CloseReason::Cancelled).unwrap();
        assert_eq!(a.retained_send_records, 1);
        assert_eq!(a.peers[&PeerId(1)].tx.dispatched.bytes, dispatched as u64);
    }
}

#[test]
fn asymmetric_demand_uses_free_pool_then_reclaims_for_a_new_consuming_peer() {
    let mut config = profile(true);
    config.max_peers = 4;
    config.receive_budget = 32 << 20;
    let mut server = Manager::new(config).unwrap();
    let mut clients = BTreeMap::new();
    let mut keys = BTreeMap::new();
    fn drive(server: &mut Manager, clients: &mut BTreeMap<PeerId, Manager>, now: u64) {
        for client in clients.values_mut() {
            forward(client, server, now);
        }
        for event in server.poll_events(256) {
            match event.event {
                Event::IncomingOpen { .. } => server.accept(event.key, b"").unwrap(),
                Event::Data { offset, bytes } => server
                    .consume_through(event.key, offset + bytes.len() as u64)
                    .unwrap(),
                _ => {}
            }
        }
        for frame in server.poll_frames(256) {
            clients
                .get_mut(&frame.key.peer)
                .unwrap()
                .receive(frame.key, &frame.frame, now)
                .unwrap();
        }
        for client in clients.values_mut() {
            client.poll_events(256);
        }
    }
    for id in 1..=3 {
        let peer = PeerId(id);
        server.register_peer(peer, true).unwrap();
        let mut client = Manager::new(profile(false)).unwrap();
        client.register_peer(peer, false).unwrap();
        keys.insert(peer, client.open(peer, b"", 0).unwrap());
        clients.insert(peer, client);
    }
    drive(&mut server, &mut clients, 0);
    for now in 1..=6 {
        for id in 1..=3 {
            let peer = PeerId(id);
            assert_eq!(
                clients
                    .get_mut(&peer)
                    .unwrap()
                    .send(keys[&peer], &[7; 16384]),
                Ok(SendOutcome::Accepted(16384))
            );
        }
        drive(&mut server, &mut clients, now);
        for id in 1..=3 {
            server
                .receive(
                    StreamKey {
                        peer: PeerId(id),
                        stream_id: 0,
                    },
                    &Frame::PeerRequest {
                        bytes: if id == 3 { 32 << 20 } else { 128 << 10 },
                        records: if id == 3 { 2112 } else { 64 },
                        probe: 0,
                        requester_stream_id: 2,
                        blocked: 0,
                    },
                    now,
                )
                .unwrap();
        }
        drive(&mut server, &mut clients, now);
    }
    assert!(server.peers[&PeerId(3)].target.bytes > 16 << 20);
    assert_eq!(server.peers[&PeerId(1)].target.bytes, 128 << 10);
    assert_eq!(server.peers[&PeerId(2)].target.bytes, 128 << 10);
    assert!(server.receive_promises.bytes <= config.receive_budget as u64 - 65536);
    let peer = PeerId(4);
    server.register_peer(peer, true).unwrap();
    let mut client = Manager::new(profile(false)).unwrap();
    client.register_peer(peer, false).unwrap();
    let key = client.open(peer, b"", 7).unwrap();
    clients.insert(peer, client);
    drive(&mut server, &mut clients, 7);
    clients
        .get_mut(&peer)
        .unwrap()
        .send(key, &[8; 16384])
        .unwrap();
    drive(&mut server, &mut clients, 8);
    server
        .receive(
            StreamKey { peer, stream_id: 0 },
            &Frame::PeerRequest {
                bytes: 1 << 20,
                records: 128,
                probe: 0,
                requester_stream_id: 2,
                blocked: 0,
            },
            8,
        )
        .unwrap();
    for now in 9..=12 {
        drive(&mut server, &mut clients, now);
    }
    assert!(server.peers[&peer].target.bytes > 65536);
    assert_eq!(server.peers[&PeerId(3)].rx.epoch, 1);
    assert!(server.peers.values().all(|peer| peer.rx.freezing.is_none()));
    assert!(server.receive_promises.bytes <= config.receive_budget as u64);
}
#[test]
fn two_saturated_peers_reclaim_idle_allowance_then_progress_without_epoch_thrash() {
    let mut config = profile(true);
    config.max_peers = 2;
    config.receive_budget = 32 << 20;
    let mut server = Manager::new(config).unwrap();
    let mut clients = Vec::new();
    let mut keys = Vec::new();
    for id in 1..=2 {
        server.register_peer(PeerId(id), true).unwrap();
        let mut client = Manager::new(profile(false)).unwrap();
        client.register_peer(PeerId(id), false).unwrap();
        let key = client.open(PeerId(id), b"", 0).unwrap();
        forward(&mut client, &mut server, 0);
        server.accept(key, b"").unwrap();
        keys.push(key);
        clients.push(client);
    }
    for frame in server.poll_frames(256) {
        clients[frame.key.peer.0 as usize - 1]
            .receive(frame.key, &frame.frame, 0)
            .unwrap();
    }
    for client in &mut clients {
        client.poll_events(256);
    }
    let mut received = [0u64; 2];
    for now in 1..80 {
        let active = if now < 12 { 1 } else { 2 };
        for i in 0..active {
            let peer = PeerId(i as u64 + 1);
            server
                .receive(
                    StreamKey { peer, stream_id: 0 },
                    &Frame::PeerRequest {
                        bytes: 32 << 20,
                        records: 2112,
                        probe: 0,
                        requester_stream_id: 2,
                        blocked: 0,
                    },
                    now,
                )
                .unwrap();
            match clients[i].send(keys[i], &[42; 16384]).unwrap() {
                SendOutcome::Accepted(_) | SendOutcome::WouldBlock => {}
            }
            forward(&mut clients[i], &mut server, now);
        }
        for event in server.poll_events(256) {
            if let Event::Data { offset, bytes } = event.event {
                let i = event.key.peer.0 as usize - 1;
                assert_eq!(offset, received[i]);
                received[i] += bytes.len() as u64;
                server
                    .consume_through(event.key, offset + bytes.len() as u64)
                    .unwrap();
            }
        }
        for frame in server.poll_frames(256) {
            clients[frame.key.peer.0 as usize - 1]
                .receive(frame.key, &frame.frame, now)
                .unwrap();
        }
        // A frozen donor may need a turn even when it submitted no new DATA.
        for client in &mut clients {
            forward(client, &mut server, now);
            client.poll_events(256);
        }
        assert!(server.receive_promises.bytes <= 32 << 20);
        assert!(server.receive_promises.records <= server.config.receive_record_budget() as u64);
    }
    assert!(received[0] > 1 << 20);
    assert!(received[1] > 512 << 10);
    for peer in [PeerId(1), PeerId(2)] {
        assert!(
            server.peers[&peer].target.bytes >= 1 << 20,
            "peer={peer:?} target={:?} epoch={} freezing={:?} consumed={} growth={} requested={:?}",
            server.peers[&peer].target,
            server.peers[&peer].rx.epoch,
            server.peers[&peer].rx.freezing,
            server.peers[&peer].rx.consumed.bytes,
            server.peers[&peer].growth_consumed,
            server.peers[&peer].requested
        );
        assert!(
            server.peers[&peer].rx.epoch <= 2,
            "unexpected repeated reclamation peer={peer:?} epoch={} target={:?} other={:?}",
            server.peers[&peer].rx.epoch,
            server.peers[&peer].target,
            server.peers[&PeerId(3 - peer.0)].target,
        );
        assert!(server.peers[&peer].rx.freezing.is_none());
    }
}

#[test]
fn failed_barrier_deadline_and_counter_boundaries_retire_only_the_affected_peer() {
    let (mut a, mut b) = pair();
    let key = established(&mut a, &mut b);
    let frame = b.peers.get_mut(&PeerId(1)).unwrap().rx.freeze().unwrap();
    b.peers.get_mut(&PeerId(1)).unwrap().freeze_deadline = Some(10);
    b.queue_control(PeerId(1), frame);
    b.tick(10).unwrap();
    assert!(b.peers[&PeerId(1)].failed);
    assert_eq!(b.receive_promises, Amount::default());
    assert_eq!(b.snapshot(key).unwrap().state, State::Closed);

    let mut b = Manager::new(profile(true)).unwrap();
    b.register_peer(PeerId(1), true).unwrap();
    b.register_peer(PeerId(2), true).unwrap();
    b.now = 2000;
    let p = b.peers.get_mut(&PeerId(1)).unwrap();
    p.rx.epoch = u64::MAX;
    p.target.bytes = 1 << 20;
    let before = p.reserved_receive;
    p.rx.grant(p.target).unwrap();
    p.reserved_receive = p.rx.promise();
    b.receive_promises = b
        .receive_promises
        .sub(before)
        .unwrap()
        .add(p.reserved_receive)
        .unwrap();
    b.peers.get_mut(&PeerId(2)).unwrap().requested.bytes = 1 << 20;
    b.reclaim_credit(PeerId(2));
    assert!(b.peers[&PeerId(1)].failed);
    assert!(!b.peers[&PeerId(2)].failed);

    let p = b.peers.get_mut(&PeerId(2)).unwrap();
    let before = p.reserved_receive;
    p.rx.received.bytes = u64::MAX - 1;
    p.rx.consumed.bytes = u64::MAX - 1;
    p.rx.allowed.bytes = u64::MAX;
    p.reserved_receive = Amount {
        bytes: 65536,
        records: 64,
    };
    p.requested.bytes = 1 << 20;
    b.receive_promises = b
        .receive_promises
        .sub(before)
        .unwrap()
        .add(p.reserved_receive)
        .unwrap();
    b.grow_credit();
    assert!(b.peers[&PeerId(2)].failed);
    assert_eq!(b.receive_promises, Amount::default());
}
