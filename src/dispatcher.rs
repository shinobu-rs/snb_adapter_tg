use std::sync::Arc;
use std::time::Duration;

use frankenstein::AsyncTelegramApi;
use frankenstein::client_reqwest::Bot;
use frankenstein::methods::{DeleteWebhookParams, GetUpdatesParams};
use frankenstein::types::AllowedUpdate;
use frankenstein::updates::Update;
use snb_core::context::BotContext;
use tokio::sync::Notify;

use crate::convert::convert_update;
use crate::state;

/// Explicit full subscription. Telegram persists a previous `allowed_updates`
/// narrowing between getUpdates calls, and the server default EXCLUDES
/// chat_member / message_reaction / message_reaction_count — so always send
/// the complete list. Revisit on frankenstein upgrades (new variants).
const ALL_UPDATES: [AllowedUpdate; 25] = [
    AllowedUpdate::Message,
    AllowedUpdate::EditedMessage,
    AllowedUpdate::ChannelPost,
    AllowedUpdate::EditedChannelPost,
    AllowedUpdate::BusinessConnection,
    AllowedUpdate::BusinessMessage,
    AllowedUpdate::EditedBusinessMessage,
    AllowedUpdate::DeletedBusinessMessages,
    AllowedUpdate::GuestMessage,
    AllowedUpdate::MessageReaction,
    AllowedUpdate::MessageReactionCount,
    AllowedUpdate::InlineQuery,
    AllowedUpdate::ChosenInlineResult,
    AllowedUpdate::CallbackQuery,
    AllowedUpdate::ShippingQuery,
    AllowedUpdate::PreCheckoutQuery,
    AllowedUpdate::PurchasedPaidMedia,
    AllowedUpdate::Poll,
    AllowedUpdate::PollAnswer,
    AllowedUpdate::MyChatMember,
    AllowedUpdate::ChatMember,
    AllowedUpdate::ChatJoinRequest,
    AllowedUpdate::ChatBoost,
    AllowedUpdate::RemovedChatBoost,
    AllowedUpdate::ManagedBot,
];

const POLL_TIMEOUT_SECS: u32 = 30;
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// frankenstein's `api_url` is the FULL prefix (`{api_url}/{method}`) — unlike
/// teloxide, `/bot<token>` must be appended by us. The default client's 500s
/// total timeout would let a hung send stall for minutes, so inject one with
/// sane bounds (still comfortably above the long-poll timeout).
fn build_bot(token: &str, api_url: Option<&str>) -> Bot {
    let server = api_url.unwrap_or("https://api.telegram.org");
    let url = format!("{}/bot{token}", server.trim_end_matches('/'));
    match frankenstein::reqwest::ClientBuilder::new()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(120))
        .build()
    {
        Ok(client) => Bot::builder().api_url(url).client(client).build(),
        Err(e) => {
            log::warn!("TGAdapter custom HTTP client failed ({e}); using library default");
            Bot::builder().api_url(url).build()
        }
    }
}

/// Recover per-element from a batch that failed typed deserialization.
///
/// Why this exists: `allowed_updates` is not retroactive (the first poll after
/// an upgrade can deliver kinds queued under the previous subscription) and
/// several payload enums (e.g. `ChatType`) are closed with no unknown-variant
/// fallback — one bad element otherwise fails the whole
/// `MethodResponse<Vec<Update>>` decode, and since the offset then never
/// advances, the same poison batch would replay forever at full poll rate.
/// `Error::JsonDecode` carries the raw body, so parse each element
/// individually and always advance past every element we saw.
pub(crate) fn salvage_updates(input: &str) -> (Vec<Update>, Option<i64>) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(input) else {
        return (Vec::new(), None);
    };
    let Some(items) = value.get("result").and_then(serde_json::Value::as_array) else {
        return (Vec::new(), None);
    };

    let mut updates = Vec::new();
    let mut next_offset: Option<i64> = None;
    for item in items {
        if let Some(id) = item.get("update_id").and_then(serde_json::Value::as_i64) {
            let next = id.saturating_add(1);
            next_offset = Some(next_offset.map_or(next, |cur| cur.max(next)));
        }
        match serde_json::from_value::<Update>(item.clone()) {
            Ok(update) => updates.push(update),
            Err(e) => log::warn!("TGAdapter skipping unparseable update: {e}"),
        }
    }
    (updates, next_offset)
}

pub(crate) async fn run(bot_ctx: Arc<dyn BotContext>) {
    let (token, api_url) = match state::config_snapshot() {
        Some(config) => config,
        None => {
            log::error!("bot_token not configured, adapter not starting");
            return;
        }
    };

    let api_url = api_url.as_deref().map(str::trim).filter(|s| !s.is_empty());
    if let Some(url) = api_url {
        log::info!("using custom Telegram API URL: {url}");
    }
    let tg_bot = build_bot(&token, api_url);
    state::set_telegram_bot(tg_bot.clone());
    state::clear_stop();
    crate::commands::sync_commands();

    // Best-effort: a leftover webhook makes every getUpdates fail with 409.
    let delete_webhook = DeleteWebhookParams::builder().build();
    if let Err(e) = tg_bot.delete_webhook(&delete_webhook).await {
        log::warn!("TGAdapter delete_webhook failed (continuing): {e}");
    }

    log::info!("start Telegram long-poll loop");
    let notify = state::stop_notify();
    let mut params = GetUpdatesParams::builder()
        .timeout(POLL_TIMEOUT_SECS)
        .allowed_updates(ALL_UPDATES.to_vec())
        .build();
    let mut backoff = Duration::from_secs(1);

    // No panic containment dance needed anymore: frankenstein returns Err on
    // an unreachable network instead of panicking like teloxide's Dispatcher.
    while !state::is_stopped() {
        let result = tokio::select! {
            () = notify.notified() => break,
            result = tg_bot.get_updates(&params) => result,
        };
        match result {
            Ok(response) => {
                backoff = Duration::from_secs(1);
                for update in response.result {
                    params.offset = Some(i64::from(update.update_id) + 1);
                    if let Some(event) = convert_update(&update) {
                        bot_ctx.emit_event(event);
                    }
                }
            }
            Err(frankenstein::Error::Api(e)) => match e.error_code {
                401 | 404 => {
                    log::error!(
                        "TGAdapter fatal: Telegram rejected the token ({}): {}",
                        e.error_code,
                        e.description
                    );
                    break;
                }
                409 => {
                    log::error!(
                        "TGAdapter getUpdates conflict (another poller or webhook active): {}",
                        e.description
                    );
                    sleep_or_stop(&notify, Duration::from_secs(60)).await;
                }
                429 => {
                    let wait = e
                        .parameters
                        .as_ref()
                        .and_then(|p| p.retry_after)
                        .unwrap_or(5);
                    log::warn!("TGAdapter getUpdates rate-limited; sleeping {wait}s");
                    sleep_or_stop(&notify, Duration::from_secs(u64::from(wait))).await;
                }
                code => {
                    log::error!("TGAdapter getUpdates API error {code}: {}", e.description);
                    sleep_or_stop(&notify, backoff).await;
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                }
            },
            Err(frankenstein::Error::JsonDecode { source, input }) => {
                log::warn!("TGAdapter getUpdates decode error ({source}); salvaging batch");
                let (updates, next_offset) = salvage_updates(&input);
                let salvaged = updates.len();
                for update in updates {
                    if let Some(event) = convert_update(&update) {
                        bot_ctx.emit_event(event);
                    }
                }
                if let Some(offset) = next_offset {
                    params.offset = Some(offset);
                    backoff = Duration::from_secs(1);
                    log::warn!("TGAdapter salvaged {salvaged} update(s); offset -> {offset}");
                } else {
                    // Not a getUpdates body at all (proxy error page etc.):
                    // treat as transport failure.
                    sleep_or_stop(&notify, backoff).await;
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                }
            }
            Err(e) => {
                log::error!("TGAdapter getUpdates transport error: {e}");
                sleep_or_stop(&notify, backoff).await;
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
        }
    }
    log::info!("Telegram long-poll loop stopped");
}

/// Backoff sleep that a stop signal can interrupt, so unload never waits on it.
async fn sleep_or_stop(notify: &Notify, duration: Duration) {
    tokio::select! {
        () = notify.notified() => {}
        () = tokio::time::sleep(duration) => {}
    }
}

#[cfg(test)]
#[path = "../tests/unit/salvage_tests.rs"]
mod salvage_tests;
