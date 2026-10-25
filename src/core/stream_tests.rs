use super::*;

fn adaptive_pair() -> (Stream, Stream) {
    let config = Config {
        receive_window: 1 << 20,
        max_frame: 16384,
        ..Config::default()
    };
    let mut sender = Stream::new(config).unwrap();
    let mut receiver = Stream::new(config).unwrap();
    sender.initial_receive_window(65536);
    receiver.initial_receive_window(65536);
    sender.open(b"", 0).unwrap();
    receiver
        .receive(&sender.poll_frames(1).pop().unwrap(), 0)
        .unwrap();
    receiver.accept(b"").unwrap();
    sender
        .receive(&receiver.poll_frames(1).pop().unwrap(), 0)
        .unwrap();
    sender.poll_events(8);
    receiver.poll_events(8);
    (sender, receiver)
}

#[test]
fn window_updates_preserve_a_grown_window_across_repeated_transfers() {
    let (mut sender, mut receiver) = adaptive_pair();
    let grown_window = 262144;
    receiver.grant_receive_window(grown_window, 1).unwrap();
    sender
        .receive(&receiver.poll_frames(1).pop().unwrap(), 1)
        .unwrap();
    let mut consumed = 0;
    for round in 0..12 {
        let mut accepted = 0;
        while let SendOutcome::Accepted(count) = sender.send(&[7; 16384]).unwrap() {
            accepted += count;
            if accepted == grown_window as usize {
                break;
            }
        }
        assert_eq!(
            accepted, grown_window as usize,
            "window fell back in round {round}"
        );
        assert_eq!(sender.send(b"x"), Ok(SendOutcome::WouldBlock));
        for frame in sender.poll_frames(256) {
            receiver.receive(&frame, round + 2).unwrap();
        }
        for event in receiver.poll_events(256) {
            if let Event::Data { offset, bytes } = event {
                assert_eq!(offset, consumed);
                assert!(bytes.iter().all(|byte| *byte == 7));
                consumed += bytes.len() as u64;
            }
        }
        receiver.consume_through(consumed).unwrap();
        let updates = receiver.poll_frames(256);
        assert!(matches!(updates.as_slice(), [Frame::WindowUpdate { .. }]));
        for frame in updates {
            sender.receive(&frame, round + 2).unwrap();
        }
        assert_eq!(sender.snapshot().send_unacknowledged_bytes, 0);
    }
    assert_eq!(consumed, 12 * grown_window as u64);
}

#[test]
fn queued_window_grant_tracks_consumption_before_dispatch() {
    let (mut sender, mut receiver) = adaptive_pair();
    for _ in 0..2 {
        assert_eq!(sender.send(&[7; 16384]), Ok(SendOutcome::Accepted(16384)));
    }
    for frame in sender.poll_frames(8) {
        receiver.receive(&frame, 1).unwrap();
    }
    assert_eq!(receiver.poll_events(8).len(), 2);
    receiver.consume_through(16384).unwrap();
    receiver.grant_receive_window(262144, 7).unwrap();
    receiver.consume_through(32768).unwrap();
    let frames = receiver.poll_frames(8);
    assert_eq!(
        frames,
        [Frame::WindowGrant {
            consumed: 32768,
            limit: 32768 + 262144,
            probe: 7,
        }]
    );
    sender.receive(&frames[0], 2).unwrap();
    assert_eq!(sender.peer_limits().unwrap().receive_window, 65536);
}

#[test]
fn standalone_tiny_data_records_remain_bounded_after_event_transfer() {
    let config = Config {
        receive_window: MAX_RECEIVE_WINDOW,
        ..Config::default()
    };
    let mut a = Stream::new(config).unwrap();
    let mut b = Stream::new(config).unwrap();
    a.open(b"", 0).unwrap();
    b.receive(&a.poll_frames(1).pop().unwrap(), 0).unwrap();
    b.accept(b"").unwrap();
    a.receive(&b.poll_frames(1).pop().unwrap(), 0).unwrap();
    a.poll_events(8);
    b.poll_events(8);
    let records = Stream::record_window(config.receive_window, config.max_frame);
    for _ in 0..records {
        assert_eq!(a.send(b"x"), Ok(SendOutcome::Accepted(1)));
        b.receive(&a.poll_frames(1).pop().unwrap(), 0).unwrap();
    }
    assert_eq!(a.send(b"x"), Ok(SendOutcome::WouldBlock));
    assert_eq!(a.sent_ends.len(), records);
    assert_eq!(b.received_ends.len(), records);
    let mut delivered = 0;
    while delivered < records {
        for event in b.poll_events(256) {
            if let Event::Data { offset, bytes } = event {
                assert_eq!(offset, delivered as u64);
                assert_eq!(&*bytes, b"x");
                delivered += bytes.len();
            }
        }
    }
    assert_eq!(b.snapshot().receive_capacity_bytes, 0);
    assert_eq!(b.received_ends.len(), records);
    assert_eq!(a.send(b"x"), Ok(SendOutcome::WouldBlock));
    b.consume_through(delivered as u64).unwrap();
    a.receive(&b.poll_frames(1).pop().unwrap(), 0).unwrap();
    assert!(a.sent_ends.is_empty());
    assert!(b.received_ends.is_empty());
    assert_eq!(a.send(b"x"), Ok(SendOutcome::Accepted(1)));
}

fn near_offset_limit() -> Stream {
    let mut stream = Stream::new(Config::default()).unwrap();
    stream.phase = Phase::Established;
    stream.peer_window = stream.config.receive_window;
    stream.peer_limit = u64::MAX;
    stream.peer_record_window =
        Stream::record_window(stream.config.receive_window, stream.config.max_frame);
    stream.receive_limit = u64::MAX;
    stream.peer_max_frame = stream.config.max_frame;
    stream.sent = u64::MAX - 1;
    stream.dispatched = u64::MAX - 1;
    stream.peer_consumed = u64::MAX - 1;
    stream.received = u64::MAX - 1;
    stream.delivered = u64::MAX - 1;
    stream.consumed = u64::MAX - 1;
    stream
}

#[test]
fn send_offset_boundary_accepts_only_the_credit_limited_prefix() {
    let mut stream = near_offset_limit();
    assert_eq!(stream.send(b"ab"), Ok(SendOutcome::Accepted(1)));
    let before = stream.snapshot();
    assert_eq!(stream.send(b"b"), Ok(SendOutcome::WouldBlock));
    assert_eq!(stream.snapshot(), before);
    stream.finish().unwrap();
    assert_eq!(
        stream.poll_frames(8),
        [
            Frame::Data {
                offset: u64::MAX - 1,
                bytes: Box::new([b'a'])
            },
            Frame::Fin {
                final_offset: u64::MAX
            },
        ]
    );
}

#[test]
fn receive_offset_exhaustion_fails_closed_without_wrapping() {
    let mut stream = near_offset_limit();
    assert_eq!(
        stream.receive(
            &Frame::Data {
                offset: u64::MAX - 1,
                bytes: Box::new([0, 1])
            },
            0,
        ),
        Err(Error::Protocol(ProtocolError::IncorrectOffset))
    );
    assert_eq!(stream.state(), State::Closed);
    assert_eq!(stream.snapshot().buffered_receive_bytes, 0);
}
