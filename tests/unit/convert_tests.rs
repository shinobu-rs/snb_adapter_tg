use super::convert_update;
use snb_core::event::{ChatType, ContentItem, EventType};

fn update(json: &str) -> frankenstein::updates::Update {
    serde_json::from_str(json).expect("test update JSON must parse")
}

#[test]
fn converts_supergroup_text_message() {
    let ev = convert_update(&update(
        r#"{"update_id":1,"message":{
            "message_id":42,"date":0,
            "chat":{"id":-1001234,"type":"supergroup","title":"T","username":"tchan"},
            "from":{"id":7,"is_bot":false,"first_name":"你","last_name":"好","language_code":"zh-hans","is_premium":true},
            "text":"hello"}}"#,
    ))
    .expect("event");
    assert!(matches!(ev.event_type, EventType::Message));
    let msg = ev.message.expect("message");
    assert_eq!(msg.chat.id, "-1001234");
    assert_eq!(msg.chat.kind, Some(ChatType::Group));
    assert_eq!(msg.chat.extra["raw_kind"], "supergroup");
    let sender = msg.sender.expect("sender");
    assert_eq!(sender.id, "7");
    assert_eq!(sender.display_name.as_deref(), Some("你 好"));
    assert_eq!(sender.extra["is_premium"], "true");
    assert_eq!(msg.id.as_deref(), Some("42"));
}

#[test]
fn extracts_command_with_cjk_prefix_via_utf16_offsets() {
    // "你好 /start@my_bot arg": bot_command entity at UTF-16 (3,13).
    let ev = convert_update(&update(
        r#"{"update_id":1,"message":{
            "message_id":1,"date":0,
            "chat":{"id":5,"type":"private"},
            "from":{"id":7,"is_bot":false,"first_name":"u"},
            "text":"你好 /start@my_bot arg",
            "entities":[{"type":"bot_command","offset":3,"length":13}]}}"#,
    ))
    .expect("event");
    assert!(matches!(ev.event_type, EventType::Command));
    let cmd = ev.command.expect("command");
    assert_eq!(cmd.cmd, "start");
    assert_eq!(cmd.args, "arg");
}

#[test]
fn collects_mentions_after_emoji() {
    // "🤝🤝 @alice ok": mention entity at UTF-16 (5,6).
    let ev = convert_update(&update(
        r#"{"update_id":1,"message":{
            "message_id":1,"date":0,
            "chat":{"id":5,"type":"private"},
            "text":"🤝🤝 @alice ok",
            "entities":[{"type":"mention","offset":5,"length":6}]}}"#,
    ))
    .expect("event");
    assert_eq!(ev.message.expect("message").at, vec!["@alice"]);
}

#[test]
fn caption_command_on_media_message() {
    let ev = convert_update(&update(
        r#"{"update_id":1,"message":{
            "message_id":1,"date":0,
            "chat":{"id":5,"type":"private"},
            "document":{"file_id":"F1","file_unique_id":"U1","file_name":"a.zip"},
            "caption":"/extract now",
            "caption_entities":[{"type":"bot_command","offset":0,"length":8}]}}"#,
    ))
    .expect("event");
    assert!(matches!(ev.event_type, EventType::Command));
    assert_eq!(ev.command.expect("command").cmd, "extract");
    let content = ev.message.expect("message").content;
    assert!(content.iter().any(|c| matches!(c, ContentItem::File { file_id: Some(id), .. } if id == "F1")));
}

#[test]
fn guest_message_routes_as_message() {
    let ev = convert_update(&update(
        r#"{"update_id":1,"guest_message":{
            "message_id":9,"date":0,
            "chat":{"id":5,"type":"private"},
            "text":"hi"}}"#,
    ))
    .expect("event");
    assert!(matches!(ev.event_type, EventType::Message));
    assert_eq!(ev.data, "GuestMessage");
}

#[test]
fn chat_join_request_passes_through_with_query_id() {
    let ev = convert_update(&update(
        r#"{"update_id":1,"chat_join_request":{
            "chat":{"id":-100,"type":"supergroup"},
            "from":{"id":7,"is_bot":false,"first_name":"u"},
            "user_chat_id":7,"date":0,"query_id":"q123"}}"#,
    ))
    .expect("event");
    match &ev.event_type {
        EventType::Other(kind) => assert_eq!(kind, "ChatJoinRequest"),
        other => panic!("expected Other, got {other:?}"),
    }
    assert!(ev.data.contains("\"query_id\":\"q123\""));
    assert_eq!(ev.reply_plugin.as_deref(), Some("TGAdapter"));
}

#[test]
fn rich_message_yields_other_content_item() {
    // A 10.1 rich message can have neither text nor caption; ensure the event
    // still carries content instead of being empty.
    let ev = convert_update(&update(
        r#"{"update_id":1,"message":{
            "message_id":1,"date":0,
            "chat":{"id":5,"type":"private"},
            "rich_message":{"blocks":[]}}}"#,
    ))
    .expect("event");
    let content = ev.message.expect("message").content;
    assert!(content.iter().any(|c| matches!(c, ContentItem::Other { kind, .. } if kind == "rich_message")));
}

#[test]
fn largest_photo_size_wins() {
    let ev = convert_update(&update(
        r#"{"update_id":1,"message":{
            "message_id":1,"date":0,
            "chat":{"id":5,"type":"private"},
            "photo":[
                {"file_id":"small","file_unique_id":"s","width":90,"height":90},
                {"file_id":"big","file_unique_id":"b","width":800,"height":600}]}}"#,
    ))
    .expect("event");
    let content = ev.message.expect("message").content;
    assert!(content.iter().any(|c| matches!(c, ContentItem::Image { file_id: Some(id), .. } if id == "big")));
}
