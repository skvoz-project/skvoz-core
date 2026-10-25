use super::*;
use std::time::Duration;
fn prefix(sequence: u64, bytes: u64) -> Prefix {
    Prefix {
        sequence,
        amount: Amount {
            bytes,
            records: sequence,
        },
    }
}
#[test]
fn terminal_fence_proves_both_directions_before_releasing_both_debts() {
    let deadline = Instant::now() + Duration::from_secs(3);
    let (a_prefix, b_prefix) = (prefix(3, 5000), prefix(2, 3000));
    let mut a = Closing::initiate(7, a_prefix, deadline);
    assert_eq!(a.output(0, Amount::default(), false).unwrap(), Some(12));
    let mut b = Closing::respond(7, b_prefix, a_prefix, deadline);
    assert_eq!(b.output(2, prefix(2, 2000).amount, true).unwrap(), None);
    assert_eq!(b.output(3, a_prefix.amount, false).unwrap(), None);
    assert_eq!(b.output(3, a_prefix.amount, true).unwrap(), Some(13));
    assert!(!b.complete());
    assert_eq!(a.acknowledged(7, b_prefix).unwrap(), a_prefix.amount);
    assert_eq!(a.output(1, prefix(1, 1000).amount, true).unwrap(), None);
    assert_eq!(a.output(2, b_prefix.amount, true).unwrap(), Some(14));
    assert!(a.complete());
    assert!(!b.complete());
    assert_eq!(b.done(7).unwrap(), b_prefix.amount);
    assert!(b.complete());
    assert_eq!(a.deadline, deadline);
    assert_eq!(b.deadline, deadline);
}
#[test]
fn missing_prefix_or_owner_never_completes_a_terminal_fence() {
    let mut state = Closing::respond(1, prefix(1, 10), prefix(2, 20), Instant::now());
    assert_eq!(state.output(1, prefix(1, 10).amount, true).unwrap(), None);
    assert_eq!(state.output(2, prefix(2, 20).amount, false).unwrap(), None);
    assert!(state.output(2, prefix(2, 19).amount, true).is_err());
    assert!(state.output(3, prefix(3, 30).amount, true).is_err());
    assert!(state.done(1).is_err());
    assert!(!state.complete());
}
#[test]
fn forged_terminal_ack_or_conflicting_prefix_cannot_release_ownership() {
    let mut state = Closing::initiate(1, prefix(1, 10), Instant::now());
    assert!(state.acknowledged(1, prefix(2, 20)).is_err());
    state.output(0, Amount::default(), true).unwrap();
    assert!(state.acknowledged(2, prefix(2, 20)).is_err());
    state.acknowledged(1, prefix(2, 20)).unwrap();
    assert!(state.acknowledged(1, prefix(2, 21)).is_err());
    assert!(!state.complete());
}
