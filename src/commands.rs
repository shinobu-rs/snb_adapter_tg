use frankenstein::AsyncTelegramApi;
use frankenstein::methods::SetMyCommandsParams;
use frankenstein::types::{BotCommand, BotCommandScope, BotCommandScopeChat};
use snb_core::command::{CommandSpec, CommandVisibility};

use crate::state;

/// Commands grouped by the Telegram scope they belong in.
pub(crate) struct MenuCommands {
    /// Shown to everyone via `BotCommandScope::Default` — public commands only.
    pub default: Vec<BotCommand>,
    /// Shown to each admin via a per-chat scope — public + admin commands.
    pub admin: Vec<BotCommand>,
}

/// Telegram requires command names to be 1–32 chars, lowercase ASCII letters,
/// digits, or underscores, and to start with a letter.
pub(crate) fn is_valid_tg_command(name: &str) -> bool {
    let len = name.chars().count();
    if !(1..=32).contains(&len) {
        return false;
    }
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_lowercase() => {}
        _ => return false,
    }
    name.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Split commands into the list shown to everyone (`Default` scope, public
/// commands only) and the list shown to admins (`Chat` scope, public + admin
/// commands). `Hidden` commands and names Telegram would reject are excluded.
///
/// The admin list intentionally includes public commands: a `Chat`-scoped list
/// replaces — does not merge with — the `Default` list for that chat.
pub(crate) fn partition_commands(specs: &[CommandSpec]) -> MenuCommands {
    let mut menu = MenuCommands {
        default: Vec::new(),
        admin: Vec::new(),
    };
    for spec in specs {
        if !is_valid_tg_command(&spec.name) {
            log::warn!(
                "TGAdapter skipping command '{}' (not a valid Telegram command name)",
                spec.name
            );
            continue;
        }
        let command = BotCommand::builder()
            .command(spec.name.clone())
            .description(spec.description.clone())
            .build();
        match spec.visibility {
            CommandVisibility::Public => {
                menu.default.push(command.clone());
                menu.admin.push(command);
            }
            CommandVisibility::Admin => menu.admin.push(command),
            CommandVisibility::Hidden => {}
        }
    }
    menu
}

/// Push the current command set to Telegram: public commands to the default
/// scope, and public + admin commands scoped to each configured admin's private
/// chat. Fire-and-forget; errors are logged. Safe to call before the bot is
/// initialized (no-op until `state::set_telegram_bot`).
pub(crate) fn sync_commands() {
    let Some(bot) = state::telegram_bot() else {
        return;
    };
    let specs = snb_core::context::bot().commands();
    let menu = partition_commands(&specs);
    let admin_ids = state::admin_ids();

    snb_core::task::spawn(async move {
        let params = SetMyCommandsParams::builder()
            .commands(menu.default)
            .scope(BotCommandScope::Default)
            .build();
        if let Err(e) = bot.set_my_commands(&params).await {
            log::warn!("TGAdapter set_my_commands (default scope) failed: {e}");
        }

        for id in admin_ids {
            let Ok(chat_id) = id.parse::<i64>() else {
                log::warn!("TGAdapter skipping non-numeric admin id '{id}' for command scope");
                continue;
            };
            let scope = BotCommandScope::Chat(
                BotCommandScopeChat::builder().chat_id(chat_id).build(),
            );
            let params = SetMyCommandsParams::builder()
                .commands(menu.admin.clone())
                .scope(scope)
                .build();
            if let Err(e) = bot.set_my_commands(&params).await {
                // A "chat not found" here means the admin has not started a DM
                // with the bot yet; the next sync (or their first message) fixes it.
                log::warn!("TGAdapter set_my_commands for admin {chat_id} failed: {e}");
            }
        }
    });
}

#[cfg(test)]
#[path = "../tests/unit/commands_tests.rs"]
mod commands_tests;
