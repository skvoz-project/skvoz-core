use super::*;
const POOL: Amount = Amount {
    bytes: 1024,
    records: 4,
};
fn pair() -> (SendCredit, ReceiveCredit) {
    let mut receive = ReceiveCredit::default();
    receive.grant(POOL).unwrap();
    let mut send = SendCredit::default();
    send.grant(0, receive.consumed, receive.allowed).unwrap();
    (send, receive)
}
#[test]
fn bytes_and_records_gate_pending_dispatched_and_actual_consumption() {
    let (mut tx, mut rx) = pair();
    for _ in 0..4 {
        tx.reserve(Amount::data(1)).unwrap();
    }
    assert_eq!(tx.room().records, 0);
    assert!(tx.reserve(Amount::data(1)).is_err());
    tx.dispatch(Amount::data(1)).unwrap();
    assert_eq!(tx.room().records, 0);
    assert!(rx.consume(Amount::data(1)).is_err());
    rx.receive(Amount::data(1)).unwrap();
    assert_eq!(rx.promise(), POOL);
    rx.consume(Amount::data(1)).unwrap();
    rx.grant(POOL).unwrap();
    tx.grant(0, rx.consumed, rx.allowed).unwrap();
    assert_eq!(tx.room().records, 1);
    assert_eq!(rx.promise(), POOL);
}
#[test]
fn cancelled_unsent_reservations_do_not_consume_unarrived_promises() {
    let (mut tx, mut rx) = pair();
    tx.reserve(Amount::data(100)).unwrap();
    tx.release_pending(Amount::data(100)).unwrap();
    assert_eq!(tx.room(), POOL);
    assert_eq!(rx.promise(), POOL);
    assert!(rx.consume(Amount::data(100)).is_err());
    // Late DATA remains constrained even when its stream has been reaped.
    rx.receive(Amount::data(100)).unwrap();
    rx.consume(Amount::data(100)).unwrap();
    assert_eq!(
        rx.promise(),
        Amount {
            bytes: 924,
            records: 3
        }
    );
}
#[test]
fn barrier_cannot_overtake_pending_data_and_only_ack_reclaims_unused_credit() {
    let (mut tx, mut rx) = pair();
    tx.reserve(Amount::data(100)).unwrap();
    let Frame::PeerFreeze { epoch } = rx.freeze().unwrap() else {
        unreachable!()
    };
    tx.freeze(epoch).unwrap();
    assert_eq!(tx.room(), Amount::default());
    assert!(tx.frozen_frame().is_none());
    assert_eq!(rx.promise(), POOL);
    assert!(
        rx.frozen(epoch, Amount::data(100), Amount::default())
            .is_err()
    );
    tx.dispatch(Amount::data(100)).unwrap();
    rx.receive(Amount::data(100)).unwrap();
    let Frame::PeerFrozen { bytes, records, .. } = tx.frozen_frame().unwrap() else {
        unreachable!()
    };
    rx.frozen(
        epoch,
        Amount { bytes, records },
        Amount {
            bytes: 10,
            records: 1,
        },
    )
    .unwrap();
    assert_eq!(
        rx.promise(),
        Amount {
            bytes: 110,
            records: 2
        }
    );
    tx.grant(epoch, rx.consumed, rx.allowed).unwrap();
    assert_eq!(
        tx.room(),
        Amount {
            bytes: 10,
            records: 1
        }
    );
    assert!(tx.grant(epoch + 1, rx.consumed, rx.allowed).is_err());
}
#[test]
fn a_new_epoch_cannot_regress_actual_consumption() {
    let (mut tx, mut rx) = pair();
    tx.reserve(Amount::data(100)).unwrap();
    tx.dispatch(Amount::data(100)).unwrap();
    rx.receive(Amount::data(100)).unwrap();
    rx.consume(Amount::data(100)).unwrap();
    rx.grant(POOL).unwrap();
    tx.grant(0, rx.consumed, rx.allowed).unwrap();
    rx.freeze().unwrap();
    tx.freeze(1).unwrap();
    tx.frozen_frame().unwrap();
    rx.frozen(1, tx.dispatched, POOL).unwrap();
    assert!(tx.grant(1, Amount::default(), rx.allowed).is_err());
    assert_eq!(tx.room(), Amount::default());
    tx.grant(1, rx.consumed, rx.allowed).unwrap();
    assert_eq!(tx.room(), POOL);
}
#[test]
fn malformed_duplicate_stale_and_overflow_counters_do_not_mint_credit() {
    let (mut tx, mut rx) = pair();
    tx.grant(0, rx.consumed, rx.allowed).unwrap();
    assert_eq!(tx.room(), POOL);
    assert!(tx.grant(0, Amount::data(1), rx.allowed).is_err());
    assert!(
        rx.receive(Amount {
            bytes: 1025,
            records: 1
        })
        .is_err()
    );
    assert_eq!(rx.received, Amount::default());
    rx.allowed = Amount {
        bytes: u64::MAX,
        records: u64::MAX,
    };
    rx.received = rx.allowed;
    assert!(rx.receive(Amount::data(1)).is_err());
    assert_eq!(rx.received, rx.allowed);
}
