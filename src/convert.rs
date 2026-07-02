use std::collections::BTreeMap;

use frankenstein::types::{ChatType as TgChatType, MessageEntityType};
use frankenstein::updates::{Update, UpdateContent};
use snb_core::event::{
    Chat, ChatType, ContentItem, Event, EventType, FileSource, ImageSource, Message, Sender,
};

use crate::state;
use crate::utf16::Utf16Index;

pub(crate) fn convert_update(update: &Update) -> Option<Event> {
    match &update.content {
        UpdateContent::Message(msg) => convert_message("Message", msg),
        UpdateContent::EditedMessage(msg) => convert_message("EditedMessage", msg),
        UpdateContent::ChannelPost(msg) => convert_message("ChannelPost", msg),
        UpdateContent::EditedChannelPost(msg) => convert_message("EditedChannelPost", msg),
        UpdateContent::BusinessMessage(msg) => convert_message("BusinessMessage", msg),
        UpdateContent::EditedBusinessMessage(msg) => convert_message("EditedBusinessMessage", msg),
        // Bot API 10.0: messages from chats the bot is not a member of. Routed
        // like a normal message (user decision) so commands work in guest chats.
        UpdateContent::GuestMessage(msg) => convert_message("GuestMessage", msg),
        content => {
            let kind_name = other_kind_name(content);
            // Externally-tagged snake_case JSON, e.g. {"chat_join_request":{...}}.
            // No consumer existed for the teloxide-era shape (verified), so the
            // serialization change is not a break.
            let data = serde_json::to_string(content).unwrap_or_default();
            Some(Event {
                event_type: EventType::Other(kind_name.to_string()),
                source: "tg-adapter".to_string(),
                data,
                command: None,
                message: None,
                reply_plugin: Some("TGAdapter".to_string()),
                target_plugin: None,
            })
        }
    }
}

fn other_kind_name(content: &UpdateContent) -> &'static str {
    match content {
        UpdateContent::Message(_)
        | UpdateContent::EditedMessage(_)
        | UpdateContent::ChannelPost(_)
        | UpdateContent::EditedChannelPost(_)
        | UpdateContent::BusinessMessage(_)
        | UpdateContent::EditedBusinessMessage(_)
        | UpdateContent::GuestMessage(_) => unreachable!(),
        UpdateContent::BusinessConnection(_) => "BusinessConnection",
        UpdateContent::DeletedBusinessMessages(_) => "DeletedBusinessMessages",
        UpdateContent::MessageReaction(_) => "MessageReaction",
        UpdateContent::MessageReactionCount(_) => "MessageReactionCount",
        UpdateContent::InlineQuery(_) => "InlineQuery",
        UpdateContent::ChosenInlineResult(_) => "ChosenInlineResult",
        UpdateContent::CallbackQuery(_) => "CallbackQuery",
        UpdateContent::ShippingQuery(_) => "ShippingQuery",
        UpdateContent::PreCheckoutQuery(_) => "PreCheckoutQuery",
        UpdateContent::PurchasedPaidMedia(_) => "PurchasedPaidMedia",
        UpdateContent::Poll(_) => "Poll",
        UpdateContent::PollAnswer(_) => "PollAnswer",
        UpdateContent::MyChatMember(_) => "MyChatMember",
        UpdateContent::ChatMember(_) => "ChatMember",
        UpdateContent::ChatJoinRequest(_) => "ChatJoinRequest",
        UpdateContent::ChatBoost(_) => "ChatBoost",
        UpdateContent::RemovedChatBoost(_) => "RemovedChatBoost",
        UpdateContent::ManagedBot(_) => "ManagedBot",
        // UpdateContent is not #[non_exhaustive] in 0.50.1; if a crate upgrade
        // adds variants the compiler will point here.
    }
}

fn convert_attachments(msg: &frankenstein::types::Message) -> Vec<ContentItem> {
    let mut items = Vec::new();

    if let Some(document) = msg.document.as_deref() {
        items.push(ContentItem::File {
            source: FileSource::Id(document.file_id.clone()),
            file_name: document.file_name.clone(),
            file_id: Some(document.file_id.clone()),
        });
    }

    if let Some(photo) = msg
        .photo
        .as_ref()
        .and_then(|photos| {
            photos
                .iter()
                .max_by_key(|p| u64::from(p.width) * u64::from(p.height))
        })
    {
        items.push(ContentItem::Image {
            source: ImageSource::Id(photo.file_id.clone()),
            file_id: Some(photo.file_id.clone()),
            caption: msg.caption.clone(),
        });
    }

    if let Some(audio) = msg.audio.as_deref() {
        items.push(ContentItem::File {
            source: FileSource::Id(audio.file_id.clone()),
            file_name: audio.file_name.clone(),
            file_id: Some(audio.file_id.clone()),
        });
    }

    if let Some(video) = msg.video.as_deref() {
        items.push(ContentItem::File {
            source: FileSource::Id(video.file_id.clone()),
            file_name: video.file_name.clone(),
            file_id: Some(video.file_id.clone()),
        });
    }

    if let Some(animation) = msg.animation.as_deref() {
        items.push(ContentItem::File {
            source: FileSource::Id(animation.file_id.clone()),
            file_name: animation.file_name.clone(),
            file_id: Some(animation.file_id.clone()),
        });
    }

    if let Some(voice) = msg.voice.as_deref() {
        items.push(ContentItem::File {
            source: FileSource::Id(voice.file_id.clone()),
            file_name: Some("voice.ogg".to_string()),
            file_id: Some(voice.file_id.clone()),
        });
    }

    // Bot API 10.1/10.0 content kinds with no snb_core representation yet:
    // forward raw JSON so such messages are not silently empty.
    if let Some(rich) = msg.rich_message.as_deref() {
        items.push(ContentItem::Other {
            kind: "rich_message".to_string(),
            data: serde_json::to_string(rich).unwrap_or_default(),
        });
    }
    if let Some(live) = msg.live_photo.as_deref() {
        items.push(ContentItem::Other {
            kind: "live_photo".to_string(),
            data: serde_json::to_string(live).unwrap_or_default(),
        });
    }

    items
}

fn convert_message(kind_name: &str, msg: &frankenstein::types::Message) -> Option<Event> {
    // Telegram messages carry text XOR caption; entities pair with whichever
    // is present (commands in media captions work through the caption pair).
    let (text, entities): (&str, &[frankenstein::types::MessageEntity]) =
        if msg.text.is_some() {
            (
                msg.text.as_deref().unwrap_or(""),
                msg.entities.as_deref().unwrap_or(&[]),
            )
        } else {
            (
                msg.caption.as_deref().unwrap_or(""),
                msg.caption_entities.as_deref().unwrap_or(&[]),
            )
        };

    let mut content = if text.is_empty() {
        vec![]
    } else {
        vec![ContentItem::text(text)]
    };
    content.extend(convert_attachments(msg));

    let from = msg.from.as_deref().map(|u| u.id.to_string());
    let chat_id = msg.chat.id.to_string();
    let (chat_type, raw_kind) = match msg.chat.type_field {
        TgChatType::Private => (ChatType::Private, "private"),
        TgChatType::Group => (ChatType::Group, "group"),
        TgChatType::Supergroup => (ChatType::Group, "supergroup"),
        TgChatType::Channel => (ChatType::Channel, "channel"),
    };

    let sender = msg.from.as_deref().map(|u| {
        let mut extra = BTreeMap::new();
        if u.is_premium == Some(true) {
            extra.insert("is_premium".to_string(), "true".to_string());
        }
        let mut full = u.first_name.clone();
        if let Some(last) = u.last_name.as_deref() {
            full.push(' ');
            full.push_str(last);
        }
        Sender {
            id: u.id.to_string(),
            username: u.username.clone(),
            display_name: (!full.is_empty()).then_some(full),
            first_name: Some(u.first_name.clone()),
            last_name: u.last_name.clone(),
            is_bot: u.is_bot,
            language: u.language_code.clone(),
            extra,
        }
    });

    let chat_extra = BTreeMap::from([("raw_kind".to_string(), raw_kind.to_string())]);
    let chat = Chat {
        id: chat_id,
        kind: Some(chat_type),
        title: msg.chat.title.clone(),
        username: msg.chat.username.clone(),
        extra: chat_extra,
    };

    // Entity offsets are UTF-16 code units; map to byte ranges once.
    let idx = Utf16Index::new(text);
    let mut at = Vec::new();
    let mut command: Option<snb_core::event::Command> = None;

    for entity in entities {
        match entity.type_field {
            MessageEntityType::Mention => {
                if let Some(r) = idx.byte_range(entity.offset, entity.length) {
                    at.push(text[r].to_string());
                }
            }
            MessageEntityType::TextMention => {
                // The mentioned user is a sibling field (not embedded in the
                // kind like teloxide); it is Option — skip defensively.
                if let Some(user) = entity.user.as_ref() {
                    at.push(user.id.to_string());
                }
            }
            MessageEntityType::BotCommand if command.is_none() => {
                if let Some(r) = idx.byte_range(entity.offset, entity.length) {
                    let raw = &text[r.clone()];
                    let stripped = raw.strip_prefix('/').unwrap_or(raw);
                    let cmd = stripped.split('@').next().unwrap_or(stripped);
                    let args = text[r.end..].trim_start();
                    command = Some(snb_core::event::Command {
                        cmd: cmd.to_string(),
                        args: args.to_string(),
                    });
                }
            }
            _ => {
                log::debug!("Unresolved message entity kind: {:?}", entity.type_field);
            }
        }
    }

    let event_msg = Message {
        id: Some(msg.message_id.to_string()),
        reply_to: msg
            .reply_to_message
            .as_deref()
            .map(|m| m.message_id.to_string()),
        content,
        sender,
        chat,
        at,
        is_admin: state::is_configured_admin(from.as_deref()),
        ..Default::default()
    };

    let (event_type, command, message) = match command {
        Some(cmd) => (EventType::Command, Some(cmd), Some(event_msg)),
        None => (EventType::Message, None, Some(event_msg)),
    };

    Some(Event {
        event_type,
        source: "tg-adapter".to_string(),
        data: kind_name.to_string(),
        command,
        message,
        reply_plugin: Some("TGAdapter".to_string()),
        target_plugin: None,
    })
}

#[cfg(test)]
#[path = "../tests/unit/convert_tests.rs"]
mod convert_tests;
