//! Telegram adapter for the Shinobu bot framework.
//!
//! Connects to the Telegram Bot API via [`teloxide`] and converts incoming
//! updates into [`Event`](snb_core::event::Event)s. Requires a bot token
//! configured in `configs/TGAdapter/config.toml`.

mod commands;
mod config;
mod convert;
mod dispatcher;
mod outgoing;
mod state;
mod task;
mod utf16;

use std::path::Path;
use std::sync::Arc;

use snb_core::adapter::{Adapter, run_async};
use snb_core::context::{self, BotContext};
use snb_core::event::{Event, EventType};
use snb_core::plugin::{PluginType, SnbPlugin, Version};
use snb_macros::plugin;

use crate::config::{Config, DEFAULT_CONFIG};

#[plugin]
struct TGAdapter;

impl SnbPlugin for TGAdapter {
    fn new() -> Self {
        Self
    }

    fn name(&self) -> &str {
        "TGAdapter"
    }

    fn version(&self) -> Version {
        Version {
            major: 0,
            minor: 0,
            patch: 1,
        }
    }

    fn plugin_type(&self) -> PluginType {
        PluginType::Adapter
    }

    fn on_load(&mut self, ctx: Arc<dyn BotContext>) {
        context::set_bot(ctx);
        context::set_plugin(self.name());

        match context::plugin().load_config(Path::new("config.toml")) {
            Ok(content) => match toml::from_str::<Config>(&content) {
                Ok(config) => state::set_config(config),
                Err(e) => log::error!("failed to parse config: {e}"),
            },
            Err(_) => {
                if let Err(e) =
                    context::plugin().write_config(Path::new("config.toml"), DEFAULT_CONFIG)
                {
                    log::error!("failed to write default config: {e}");
                }
                log::warn!(
                    "config not found, default config written to configs/TGAdapter/config.toml, please edit it with your bot token"
                );
            }
        }

        context::register_all(self.name());
        log::info!("v{} loaded!", self.version());
    }

    fn on_unload(&mut self) {
        state::reset();
        log::info!("unloaded!");
    }

    fn on_event(&self, event: &Event) {
        // Keep the Telegram command menu in sync as plugins (and their commands)
        // come and go. `sync_commands` no-ops until the bot is initialized.
        if matches!(
            event.event_type,
            EventType::PluginLoaded | EventType::PluginUnloaded
        ) {
            crate::commands::sync_commands();
        }
    }
}

#[derive(Clone, Copy)]
struct TelegramAdapter;

impl Adapter for TelegramAdapter {
    fn run(&self, bot: Arc<dyn BotContext>) {
        run_async(dispatcher::run(bot));
    }

    fn send(&self, event: &Event) -> anyhow::Result<()> {
        outgoing::send_event(event)
    }
}

snb_core::registry::submit! {
    snb_core::registry::AdapterRegistration {
        factory: || Arc::new(TelegramAdapter),
    }
}
