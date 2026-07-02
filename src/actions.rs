//! Adapter-local outgoing action channel.
//!
//! Plugins drive Telegram moderation / guard-bot methods without any
//! `snb_core` ABI surface: send an `EventType::Other("tg:<method>")` event
//! targeted at `TGAdapter` whose `data` is the Bot API params as JSON
//! (frankenstein's params structs deserialize Bot-API field names directly).
//! If `reply_plugin` is set, the outcome is emitted back to it as
//! `Other("tg:response")` with `{"method", "ok", "result"|"error"}`.

use anyhow::Context as _;
use frankenstein::AsyncTelegramApi;
use frankenstein::client_reqwest::Bot;
use snb_core::event::{Event, EventType};

use crate::state;

const SUPPORTED: [&str; 9] = [
    "answer_chat_join_request_query",
    "send_chat_join_request_web_app",
    "approve_chat_join_request",
    "decline_chat_join_request",
    "restrict_chat_member",
    "ban_chat_member",
    "unban_chat_member",
    "answer_callback_query",
    "get_chat",
];

pub(crate) fn is_supported(method: &str) -> bool {
    SUPPORTED.contains(&method)
}

pub(crate) fn dispatch(method: &str, event: &Event) -> anyhow::Result<()> {
    if !is_supported(method) {
        anyhow::bail!("TGAdapter unsupported action: {method}");
    }
    let bot = state::telegram_bot().context("TGAdapter bot not initialized")?;
    let method = method.to_string();
    let data = event.data.clone();
    let reply_plugin = event.reply_plugin.clone();
    crate::task::spawn(async move {
        run_action(bot, method, data, reply_plugin).await;
    });
    Ok(())
}

/// One arm per method: deserialize `data` into the params struct, call, and
/// normalize the outcome to (ok, JSON payload).
macro_rules! action {
    ($bot:expr, $data:expr, $params_ty:ty, $call:ident) => {{
        match serde_json::from_str::<$params_ty>(&$data) {
            Ok(params) => match $bot.$call(&params).await {
                Ok(resp) => (
                    true,
                    serde_json::to_value(&resp.result).unwrap_or(serde_json::Value::Null),
                ),
                Err(e) => (false, serde_json::Value::String(e.to_string())),
            },
            Err(e) => (
                false,
                serde_json::Value::String(format!("invalid params: {e}")),
            ),
        }
    }};
}

async fn run_action(bot: Bot, method: String, data: String, reply_plugin: Option<String>) {
    use frankenstein::methods::{
        AnswerCallbackQueryParams, AnswerChatJoinRequestQueryParams,
        ApproveChatJoinRequestParams, BanChatMemberParams, DeclineChatJoinRequestParams,
        GetChatParams, RestrictChatMemberParams, SendChatJoinRequestWebAppParams,
        UnbanChatMemberParams,
    };

    let (ok, payload) = match method.as_str() {
        "answer_chat_join_request_query" => action!(
            bot, data, AnswerChatJoinRequestQueryParams, answer_chat_join_request_query
        ),
        "send_chat_join_request_web_app" => action!(
            bot, data, SendChatJoinRequestWebAppParams, send_chat_join_request_web_app
        ),
        "approve_chat_join_request" => {
            action!(bot, data, ApproveChatJoinRequestParams, approve_chat_join_request)
        }
        "decline_chat_join_request" => {
            action!(bot, data, DeclineChatJoinRequestParams, decline_chat_join_request)
        }
        "restrict_chat_member" => {
            action!(bot, data, RestrictChatMemberParams, restrict_chat_member)
        }
        "ban_chat_member" => action!(bot, data, BanChatMemberParams, ban_chat_member),
        "unban_chat_member" => action!(bot, data, UnbanChatMemberParams, unban_chat_member),
        "answer_callback_query" => {
            action!(bot, data, AnswerCallbackQueryParams, answer_callback_query)
        }
        "get_chat" => action!(bot, data, GetChatParams, get_chat),
        // dispatch() already rejected anything else. Fall through to the
        // normal (ok, payload) path below instead of returning early, so the
        // `if !ok` log (single error log, no double-logging) and the
        // reply_plugin response-emission still run — otherwise a requesting
        // plugin that set reply_plugin would wait forever on a desync.
        other => (
            false,
            serde_json::Value::String(format!(
                "no handler for '{other}' (whitelist/handler desync)"
            )),
        ),
    };

    if !ok {
        log::error!("TGAdapter action '{method}' failed: {payload}");
    }
    let Some(target) = reply_plugin else { return };

    let mut body = serde_json::Map::new();
    body.insert("method".to_string(), method.into());
    body.insert("ok".to_string(), ok.into());
    body.insert(
        if ok { "result" } else { "error" }.to_string(),
        payload,
    );
    let event = Event {
        event_type: EventType::Other("tg:response".to_string()),
        source: "tg-adapter".to_string(),
        data: serde_json::Value::Object(body).to_string(),
        command: None,
        message: None,
        reply_plugin: Some("TGAdapter".to_string()),
        target_plugin: Some(target),
    };
    snb_core::context::bot().emit_event(event);
}

#[cfg(test)]
#[path = "../tests/unit/actions_tests.rs"]
mod actions_tests;
