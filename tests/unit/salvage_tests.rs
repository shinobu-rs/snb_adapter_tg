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
