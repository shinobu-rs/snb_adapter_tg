use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, RwLock};

use frankenstein::client_reqwest::Bot;
use tokio::sync::Notify;

use crate::config::Config;

// Plugin-wide state. Each plugin is a singleton (one cdylib, one instance), so
// module-level globals mirror the framework's own `context::set_bot` pattern and
// let the stateless adapter and plugin lifecycle share data.
// `RwLock<Option<_>>` (not `OnceLock`) so `on_unload` can reset it for reloads.
static CONFIG: RwLock<Option<Config>> = RwLock::new(None);
static TG_BOT: RwLock<Option<Bot>> = RwLock::new(None);

// Stop signaling for `Adapter::stop()`: the flag is the ground truth (checked
// at the top of the poll loop), the Notify interrupts an in-flight long poll
// so the loop exits inside the host's ~1s unload drain window.
static STOPPED: AtomicBool = AtomicBool::new(false);
static STOP_NOTIFY: OnceLock<Arc<Notify>> = OnceLock::new();

pub(crate) fn set_config(config: Config) {
    *CONFIG.write().unwrap() = Some(config);
}

pub(crate) fn config_snapshot() -> Option<(String, Option<String>)> {
    CONFIG
        .read()
        .unwrap()
        .as_ref()
        .map(|config| (config.bot_token.clone(), config.api_url.clone()))
}

pub(crate) fn is_configured_admin(user_id: Option<&str>) -> bool {
    let Some(user_id) = user_id else {
        return false;
    };
    CONFIG
        .read()
        .unwrap()
        .as_ref()
        .is_some_and(|config| config.is_admin(user_id))
}

pub(crate) fn admin_ids() -> Vec<String> {
    CONFIG
        .read()
        .unwrap()
        .as_ref()
        .map(Config::admin_ids)
        .unwrap_or_default()
}

pub(crate) fn set_telegram_bot(bot: Bot) {
    *TG_BOT.write().unwrap() = Some(bot);
}

pub(crate) fn telegram_bot() -> Option<Bot> {
    TG_BOT.read().unwrap().as_ref().cloned()
}

pub(crate) fn stop_notify() -> Arc<Notify> {
    STOP_NOTIFY.get_or_init(|| Arc::new(Notify::new())).clone()
}

/// Called from `Adapter::stop()` (host thread). Must not block.
pub(crate) fn signal_stop() {
    STOPPED.store(true, Ordering::SeqCst);
    stop_notify().notify_waiters();
}

pub(crate) fn is_stopped() -> bool {
    STOPPED.load(Ordering::SeqCst)
}

/// Reset the stop flag at adapter startup (a reload reuses the statics).
pub(crate) fn clear_stop() {
    STOPPED.store(false, Ordering::SeqCst);
}

pub(crate) fn reset() {
    *TG_BOT.write().unwrap() = None;
    *CONFIG.write().unwrap() = None;
}
