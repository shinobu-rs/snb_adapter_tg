use std::sync::Arc;
use std::time::Duration;

use frankenstein::AsyncTelegramApi;
use frankenstein::client_reqwest::Bot;
use frankenstein::methods::{DeleteWebhookParams, GetUpdatesParams};
use frankenstein::types::AllowedUpdate;
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
