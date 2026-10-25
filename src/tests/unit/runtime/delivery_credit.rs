use super::*;

fn amount(bytes: u64, records: u64) -> Amount {
    Amount { bytes, records }
}

#[test]
fn refill_overlaps_publication_without_a_new_demand_round() {
    let mut rx = Recipient::default();
    let mut tx = Sender::default();
    rx.extend(amount(4096, 4)).unwrap();
    tx.grant(rx.snapshot()).unwrap();
    for _ in 0..128 {
        tx.reserve(amount(1024, 1)).unwrap();
        tx.commit(amount(1024, 1)).unwrap();
        rx.apply(amount(1024, 1)).unwrap();
        rx.extend(amount(1024, 1)).unwrap();
        assert_eq!(rx.obligation(), amount(4096, 4));
        assert!(tx.room().contains(amount(1024, 1)));
        tx.grant(rx.snapshot()).unwrap();
        assert_eq!(tx.room(), amount(4096, 4));
    }
    assert_eq!(tx.sent, amount(131072, 128));
    assert_eq!(tx.outstanding(), Amount::default());
}

#[test]
fn uncommitted_reservations_can_be_cancelled_but_sent_bytes_cannot() {
    let mut tx = Sender::default();
    tx.grant(Limit {
        allowed: amount(10, 2),
        ..Limit::default()
    })
    .unwrap();
    tx.reserve(amount(8, 1)).unwrap();
    assert!(!tx.room().contains(amount(3, 1)));
    tx.cancel_pending(amount(8, 1)).unwrap();
    tx.reserve(amount(8, 1)).unwrap();
    tx.commit(amount(8, 1)).unwrap();
    assert!(tx.cancel_pending(amount(8, 1)).is_err());
    assert_eq!(tx.outstanding(), amount(8, 1));
}

#[test]
fn early_frozen_control_never_reuses_data_still_in_flight() {
    let mut rx = Recipient::default();
    let mut tx = Sender::default();
    rx.extend(amount(4096, 4)).unwrap();
    tx.grant(rx.snapshot()).unwrap();
    tx.reserve(amount(1024, 1)).unwrap();
    let epoch = rx.freeze().unwrap();
    tx.freeze(epoch).unwrap();
    assert!(tx.frozen_prefix().is_none());
    assert!(tx.reserve(amount(1, 1)).is_err());
    tx.commit(amount(1024, 1)).unwrap();
    rx.observe_frozen(epoch, tx.frozen_prefix().unwrap())
        .unwrap();
    assert_eq!(rx.reclaim().unwrap(), None);
    assert_eq!(rx.obligation(), amount(4096, 4));
    rx.apply(amount(1024, 1)).unwrap();
    assert_eq!(rx.reclaim().unwrap(), Some(amount(3072, 3)));
    tx.grant(rx.snapshot()).unwrap();
    assert_eq!(tx.room(), Amount::default());
    rx.extend(amount(512, 1)).unwrap();
    tx.grant(rx.snapshot()).unwrap();
    assert_eq!(tx.room(), amount(512, 1));
}

#[test]
fn record_limits_apply_independently_of_byte_capacity() {
    let mut rx = Recipient::default();
    rx.extend(amount(1 << 20, 1)).unwrap();
    rx.apply(amount(1, 1)).unwrap();
    assert!(rx.apply(amount(1, 1)).is_err());
    assert_eq!(rx.obligation(), amount((1 << 20) - 1, 0));
}

#[test]
fn forged_receipts_and_unrequested_epochs_fail_without_mutation() {
    let mut tx = Sender::default();
    let initial = Limit {
        allowed: amount(4096, 4),
        ..Limit::default()
    };
    tx.grant(initial).unwrap();
    for invalid in [
        Limit {
            applied: amount(1, 0),
            ..initial
        },
        Limit {
            epoch: 1,
            ..initial
        },
        Limit {
            allowed: amount(1, 4),
            ..initial
        },
    ] {
        assert!(tx.grant(invalid).is_err());
        assert_eq!(tx.allowed, initial.allowed);
        assert_eq!(tx.epoch, 0);
    }
}

#[test]
fn aggregate_promises_include_idle_and_quarantined_sources() {
    let budget = amount(8192, 8);
    let mut sources = [Recipient::default(), Recipient::default()];
    sources[0].extend(amount(4096, 4)).unwrap();
    sources[1].extend(amount(4096, 4)).unwrap();
    let total = sources[0]
        .obligation()
        .add(sources[1].obligation())
        .unwrap();
    assert_eq!(total, budget);
    // Retirement transfers the same obligation into quarantine. It cannot
    // disappear merely because the live source table no longer contains it.
    let quarantine = sources[0].obligation();
    let remaining = sources[1].obligation().add(quarantine).unwrap();
    assert_eq!(remaining, budget);
    assert!(!budget.sub(remaining).unwrap().contains(amount(1, 1)));
}

#[test]
fn bounded_control_never_requires_receipt_of_a_receipt() {
    let now = Instant::now();
    let timeout = Duration::from_secs(10);
    let mut a = ControlWindow::default();
    let mut b = ControlWindow::default();
    let seq = a.begin(now, timeout).unwrap().unwrap();
    assert!(a.begin(now, timeout).unwrap().is_none());
    assert!(b.fresh(seq).unwrap());
    // A prefix-bearing control may be received before its DATA. It is not
    // acknowledged as applied until that prefix has actually been handled.
    assert!(!b.receipt_dirty());
    b.applied(seq).unwrap();
    assert!(b.receipt_dirty());
    a.acknowledge(b.received).unwrap();
    b.mark_receipt_sent();
    assert!(a.deadline.is_none());
    assert!(b.deadline.is_none());
    assert!(!b.receipt_dirty());
    assert!(!b.fresh(seq).unwrap());
    assert_eq!(a.begin(now, timeout).unwrap(), Some(seq + 1));
}

#[test]
fn future_control_ack_and_sequence_gaps_are_protocol_errors() {
    let mut c = ControlWindow::default();
    assert!(c.acknowledge(1).is_err());
    assert!(c.fresh(0).is_err());
    assert!(c.fresh(2).is_err());
    c.begin(Instant::now(), Duration::from_secs(10)).unwrap();
    assert!(c.acknowledge(2).is_err());
    assert!(c.deadline.is_some());
}
