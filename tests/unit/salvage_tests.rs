use super::salvage_updates;

#[test]
fn salvages_good_updates_and_advances_past_poison() {
    // Second element has an unknown chat type — a closed enum with no fallback
    // variant, which poisons whole-batch typed deserialization.
    let input = r#"{"ok":true,"result":[
        {"update_id":100,"message":{"message_id":1,"date":0,"chat":{"id":5,"type":"private"},"text":"hi"}},
        {"update_id":101,"message":{"message_id":2,"date":0,"chat":{"id":5,"type":"astral_plane"},"text":"bad"}}
    ]}"#;
    let (updates, next_offset) = salvage_updates(input);
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].update_id, 100);
    // Offset must advance past the poison element too, or it replays forever.
    assert_eq!(next_offset, Some(102));
}

#[test]
fn non_json_input_yields_nothing() {
    let (updates, next_offset) = salvage_updates("<html>502 Bad Gateway</html>");
    assert!(updates.is_empty());
    assert_eq!(next_offset, None);
}

#[test]
fn missing_result_array_yields_nothing() {
    let (updates, next_offset) = salvage_updates(r#"{"ok":false}"#);
    assert!(updates.is_empty());
    assert_eq!(next_offset, None);
}

#[test]
fn huge_update_id_saturates_instead_of_panicking() {
    // update_id is attacker-influenced JSON; i64::MAX + 1 must not overflow.
    let input = r#"{"ok":true,"result":[
        {"update_id":9223372036854775807,"message":{"message_id":1,"date":0,"chat":{"id":5,"type":"private"},"text":"hi"}}
    ]}"#;
    let (_updates, next_offset) = salvage_updates(input);
    assert_eq!(next_offset, Some(i64::MAX)); // saturated, no panic
}

#[test]
fn out_of_order_ids_do_not_overshoot_offset() {
    // A protocol-violating body may deliver ids out of order; the offset must
    // still be exactly max(update_id) + 1, not inflated per element.
    let input = r#"{"ok":true,"result":[
        {"update_id":101,"message":{"message_id":1,"date":0,"chat":{"id":5,"type":"private"},"text":"a"}},
        {"update_id":100,"message":{"message_id":2,"date":0,"chat":{"id":5,"type":"private"},"text":"b"}}
    ]}"#;
    let (updates, next_offset) = salvage_updates(input);
    assert_eq!(updates.len(), 2);
    assert_eq!(next_offset, Some(102));
}
