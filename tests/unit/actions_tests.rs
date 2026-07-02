use frankenstein::methods::{
    AnswerChatJoinRequestQueryParams, GetChatParams, RestrictChatMemberParams,
};

use super::is_supported;

#[test]
fn whitelist_covers_guard_bot_and_moderation_methods() {
    for method in [
        "answer_chat_join_request_query",
        "send_chat_join_request_web_app",
        "approve_chat_join_request",
        "decline_chat_join_request",
        "restrict_chat_member",
        "ban_chat_member",
        "unban_chat_member",
        "answer_callback_query",
        "get_chat",
    ] {
        assert!(is_supported(method), "{method} should be supported");
    }
    assert!(!is_supported("send_message")); // relay path, not an action
    assert!(!is_supported("delete_webhook")); // adapter-internal only
}

// The action channel deserializes event.data straight into frankenstein params
// structs — these pin the Bot-API-field JSON contract plugins will use.
#[test]
fn params_deserialize_from_bot_api_field_json() {
    let p: AnswerChatJoinRequestQueryParams = serde_json::from_str(
        r#"{"chat_join_request_query_id":"q1","result":"approve"}"#,
    )
    .expect("answer params");
    assert_eq!(p.chat_join_request_query_id, "q1");
    assert_eq!(p.result, "approve");

    let p: RestrictChatMemberParams = serde_json::from_str(
        r#"{"chat_id":-1001234,"user_id":42,"permissions":{"can_send_messages":false}}"#,
    )
    .expect("restrict params");
    assert_eq!(p.user_id, 42);

    let p: GetChatParams =
        serde_json::from_str(r#"{"chat_id":-1001234}"#).expect("get_chat params");
    let _ = p;
}
