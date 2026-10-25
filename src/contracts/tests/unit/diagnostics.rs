use super::*;
#[test]
fn disabled_and_new_collection_do_not_report_old_timings() {
    let mut value = TurnDiagnostics::default();
    assert!(!value.enabled);
    value.set_enabled(true);
    value.turns = 10;
    value.idle_us = 123;
    value.set_enabled(true);
    assert_eq!(value.turns, 10);
    value.set_enabled(false);
    assert_eq!(value.turns, 0);
    assert_eq!(value.idle_us, 0);
    value.set_enabled(true);
    assert_eq!(value.turns, 0);
}
