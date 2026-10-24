use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::Context as _;
use base64::{Engine as _, engine::general_purpose};
use frankenstein::AsyncTelegramApi;
use frankenstein::ParseMode;
use frankenstein::client_reqwest::Bot;
use frankenstein::input_file::FileUpload;
use frankenstein::input_media::{InputMediaDocument, MediaGroupInputMedia};
use frankenstein::methods::{
    DeleteMessageParams, EditMessageTextParams, SendDocumentParams, SendMediaGroupParams,
    SendMessageParams, SendPhotoParams,
};
use frankenstein::types::ReplyParameters;
use snb_core::context;
use snb_core::event::{
    ContentItem, Event, EventType, FileSource, ImageSource, Message, TextFormat,
};

use crate::state;

pub(crate) fn send_event(event: &Event) -> anyhow::Result<()> {
    // Adapter-local action channel: Other("tg:<method>") events targeted at
    // this adapter carry Bot API params JSON in event.data.
    if let EventType::Other(kind) = &event.event_type {
        if let Some(method) = kind.strip_prefix("tg:") {
            return crate::actions::dispatch(method, event);
        }
        return Ok(());
    }
    let Some(msg) = &event.message else {
        return Ok(());
    };
    let chat_id = msg.chat_id();
    if chat_id.is_empty() {
        anyhow::bail!("TGAdapter outgoing message is missing chat id");
    }
    let chat_id = chat_id
        .parse::<i64>()
        .with_context(|| format!("invalid Telegram chat id: {chat_id}"))?;
    let bot = state::telegram_bot().context("TGAdapter bot not initialized")?;
    let event_type = event.event_type.clone();
    let origin = event.source.clone();
    let msg = msg.clone();

    snb_core::task::spawn(async move {
        match event_type {
            EventType::Message => send_message_items(bot, chat_id, msg, origin).await,
            EventType::MessageEdit => edit_message_item(bot, chat_id, msg).await,
            EventType::MessageDelete => delete_message_item(bot, chat_id, msg).await,
            kind => log::debug!("TGAdapter ignored outgoing event type: {kind:?}"),
        }
    });

    Ok(())
}

async fn send_message_items(bot: Bot, chat_id: i64, msg: Message, origin: String) {
    let local_id = msg.id.clone();
    let reply_to = msg.reply_to;
    let delete_after = msg.delete_after;
    let mut text = OutgoingText::default();
    let mut attachments = Vec::new();

    for item in msg.content {
        match item {
            ContentItem::Text {
                text: item_text,
                format,
            } => text.push_text(item_text, format),
            ContentItem::File { .. } | ContentItem::Image { .. } => attachments.push(item),
            ContentItem::Other { kind, .. } => {
                log::debug!("TGAdapter ignored unsupported outgoing content kind: {kind}");
            }
        }
    }

    let caption = text.into_payload();
    let mut sent_ids = Vec::new();
    if attachments.is_empty() {
        if let Some(text) = caption {
            sent_ids
                .extend(send_text(&bot, chat_id, text, reply_to.as_deref(), delete_after).await);
        }
        emit_message_sent_if_needed(
            &origin,
            chat_id,
            local_id.as_deref(),
            sent_ids.first().copied(),
        );
        return;
    }

    if attachments.len() > 1
        && attachments
            .iter()
            .all(|item| matches!(item, ContentItem::File { .. }))
    {
        sent_ids.extend(
            send_file_media_groups(
                &bot,
                chat_id,
                attachments,
                caption,
                reply_to.as_deref(),
                delete_after,
            )
            .await,
        );
        emit_message_sent_if_needed(
            &origin,
            chat_id,
            local_id.as_deref(),
            sent_ids.first().copied(),
        );
        return;
    }

    let mut first_caption = caption;
    for item in attachments {
        match item {
            ContentItem::File {
                source,
                file_name,
                file_id,
            } => {
                sent_ids.extend(
                    send_file(
                        &bot,
                        chat_id,
                        source,
                        file_name,
                        file_id,
                        first_caption.take(),
                        reply_to.as_deref(),
                        delete_after,
                    )
                    .await,
                );
            }
            ContentItem::Image {
                source,
                file_id,
                caption,
            } => {
                let caption = merge_captions(first_caption.take(), caption);
                sent_ids.extend(
                    send_image(
                        &bot,
                        chat_id,
                        source,
                        file_id,
                        caption,
                        reply_to.as_deref(),
                        delete_after,
                    )
                    .await,
                );
            }
            ContentItem::Text { .. } | ContentItem::Other { .. } => unreachable!(),
        }
    }

    emit_message_sent_if_needed(
        &origin,
        chat_id,
        local_id.as_deref(),
        sent_ids.first().copied(),
    );
}

#[derive(Default)]
struct OutgoingText {
    text: String,
    format: Option<TextFormat>,
    has_plain: bool,
    mixed_formats: bool,
}

#[derive(Clone)]
struct FormattedPayload {
    text: String,
    format: Option<TextFormat>,
}

impl OutgoingText {
    fn push_text(&mut self, text: String, format: Option<TextFormat>) {
        self.push(text, format);
    }

    fn push(&mut self, text: String, format: Option<TextFormat>) {
        if text.is_empty() {
            return;
        }

        match format {
            Some(format) if !self.has_plain => {
                if let Some(existing) = self.format {
                    self.mixed_formats |= existing != format;
                } else {
                    self.format = Some(format);
                }
            }
            Some(_) => {
                self.mixed_formats = true;
            }
            None => {
                if self.format.is_some() {
                    self.mixed_formats = true;
                }
                self.has_plain = true;
            }
        }

        self.text.push_str(&text);
    }

    fn into_payload(self) -> Option<FormattedPayload> {
        (!self.text.is_empty()).then_some(FormattedPayload {
            text: self.text,
            format: (!self.mixed_formats && !self.has_plain)
                .then_some(self.format)
                .flatten(),
        })
    }
}

/// Retry a send exactly once when Telegram answers 429, honoring retry_after.
/// (No general retry policy — errors still log-and-continue like before.)
macro_rules! send_with_429_retry {
    ($bot:expr, $method:ident, $params:expr) => {{
        match $bot.$method($params).await {
            Err(frankenstein::Error::Api(ref e)) if e.error_code == 429 => {
                let wait = e.parameters.as_ref().and_then(|p| p.retry_after).unwrap_or(5);
                log::warn!(
                    "TGAdapter {} rate-limited; retrying once in {wait}s",
                    stringify!($method)
                );
                tokio::time::sleep(std::time::Duration::from_secs(u64::from(wait))).await;
                $bot.$method($params).await
            }
            other => other,
        }
    }};
}

async fn send_text(
    bot: &Bot,
    chat_id: i64,
    text: FormattedPayload,
    reply_to: Option<&str>,
    delete_after: Option<Duration>,
) -> Vec<i32> {
    let parse_mode = text.format.map(parse_mode_from_format);
    let params = SendMessageParams::builder()
        .chat_id(chat_id)
        .text(&text.text)
        .maybe_parse_mode(parse_mode)
        .maybe_reply_parameters(reply_parameters(reply_to))
        .build();
    match send_with_429_retry!(bot, send_message, &params) {
        Ok(resp) => {
            let id = resp.result.message_id;
            schedule_delete_after(bot, chat_id, id, delete_after);
            vec![id]
        }
        Err(e) if parse_mode.is_some() => {
            log::warn!("TGAdapter send_message parse_mode failed ({e}); falling back to plain text");
            let fallback_params = SendMessageParams::builder()
                .chat_id(chat_id)
                .text(text.text)
                .maybe_reply_parameters(reply_parameters(reply_to))
                .build();
            match send_with_429_retry!(bot, send_message, &fallback_params) {
                Ok(resp) => {
                    let id = resp.result.message_id;
                    schedule_delete_after(bot, chat_id, id, delete_after);
                    vec![id]
                }
                Err(err) => {
                    log::error!("TGAdapter send_message fallback error: {err}");
                    Vec::new()
                }
            }
        }
        Err(e) => {
            log::error!("TGAdapter send_message error: {e}");
            Vec::new()
        }
    }
}

async fn send_file(
    bot: &Bot,
    chat_id: i64,
    source: FileSource,
    file_name: Option<String>,
    file_id: Option<String>,
    caption: Option<FormattedPayload>,
    reply_to: Option<&str>,
    delete_after: Option<Duration>,
) -> Vec<i32> {
    let (upload, cleanup) = match file_upload_from_source(source, file_name, file_id) {
        Ok(prepared) => prepared,
        Err(e) => {
            log::error!("TGAdapter cannot prepare file: {e:#}");
            return Vec::new();
        }
    };

    let (caption_text, caption_mode) = split_caption(caption);
    let params = SendDocumentParams::builder()
        .chat_id(chat_id)
        .document(upload)
        .maybe_caption(caption_text)
        .maybe_parse_mode(caption_mode)
        .maybe_reply_parameters(reply_parameters(reply_to))
        .build();
    let result = send_with_429_retry!(bot, send_document, &params);
    cleanup_outbox(cleanup.as_deref());
    match result {
        Ok(resp) => {
            let id = resp.result.message_id;
            schedule_delete_after(bot, chat_id, id, delete_after);
            vec![id]
        }
        Err(e) => {
            log::error!("TGAdapter send_document error: {e}");
            Vec::new()
        }
    }
}

async fn send_image(
    bot: &Bot,
    chat_id: i64,
    source: ImageSource,
    file_id: Option<String>,
    caption: Option<FormattedPayload>,
    reply_to: Option<&str>,
    delete_after: Option<Duration>,
) -> Vec<i32> {
    let (upload, cleanup) = match file_upload_from_image(source, file_id) {
        Ok(prepared) => prepared,
        Err(e) => {
            log::error!("TGAdapter cannot prepare image: {e:#}");
            return Vec::new();
        }
    };

    let (caption_text, caption_mode) = split_caption(caption);
    let params = SendPhotoParams::builder()
        .chat_id(chat_id)
        .photo(upload)
        .maybe_caption(caption_text)
        .maybe_parse_mode(caption_mode)
        .maybe_reply_parameters(reply_parameters(reply_to))
        .build();
    let result = send_with_429_retry!(bot, send_photo, &params);
    cleanup_outbox(cleanup.as_deref());
    match result {
        Ok(resp) => {
            let id = resp.result.message_id;
            schedule_delete_after(bot, chat_id, id, delete_after);
            vec![id]
        }
        Err(e) => {
            log::error!("TGAdapter send_photo error: {e}");
            Vec::new()
        }
    }
}

async fn send_file_media_groups(
    bot: &Bot,
    chat_id: i64,
    files: Vec<ContentItem>,
    mut caption: Option<FormattedPayload>,
    reply_to: Option<&str>,
    delete_after: Option<Duration>,
) -> Vec<i32> {
    let mut sent_ids = Vec::new();
    let mut chunk = Vec::new();
    for file in files {
        chunk.push(file);
        if chunk.len() == 10 {
            let chunk_caption = caption.take();
            sent_ids.extend(
                send_file_media_group(
                    bot,
                    chat_id,
                    std::mem::take(&mut chunk),
                    chunk_caption,
                    reply_to,
                    delete_after,
                )
                .await,
            );
        }
    }

    if !chunk.is_empty() {
        sent_ids.extend(
            send_file_media_group(bot, chat_id, chunk, caption, reply_to, delete_after).await,
        );
    }

    sent_ids
}

async fn send_file_media_group(
    bot: &Bot,
    chat_id: i64,
    files: Vec<ContentItem>,
    caption: Option<FormattedPayload>,
    reply_to: Option<&str>,
    delete_after: Option<Duration>,
) -> Vec<i32> {
    if files.len() == 1 {
        if let Some(ContentItem::File {
            source,
            file_name,
            file_id,
        }) = files.into_iter().next()
        {
            return send_file(
                bot, chat_id, source, file_name, file_id, caption, reply_to, delete_after,
            )
            .await;
        }
        return Vec::new();
    }

    let last_index = files.len() - 1;
    let mut media = Vec::with_capacity(files.len());
    let mut cleanups: Vec<PathBuf> = Vec::new();
    for (idx, file) in files.into_iter().enumerate() {
        let ContentItem::File {
            source,
            file_name,
            file_id,
        } = file
        else {
            unreachable!();
        };
        let (upload, cleanup) = match file_upload_from_source(source, file_name, file_id) {
            Ok(prepared) => prepared,
            Err(e) => {
                log::error!("TGAdapter cannot prepare file: {e:#}");
                for c in &cleanups {
                    cleanup_outbox(Some(c.as_path()));
                }
                return Vec::new();
            }
        };
        if let Some(c) = cleanup {
            cleanups.push(c);
        }
        // Album caption goes on the LAST item (preserves existing behavior).
        // NOTE: never set `thumbnail` on media-group documents — frankenstein
        // 0.50.1's attach:// replacement walk skips it and Telegram rejects
        // the raw path object (source-verified).
        let document = if idx == last_index && caption.is_some() {
            let c = caption.clone().expect("checked is_some");
            InputMediaDocument::builder()
                .media(upload)
                .caption(c.text)
                .maybe_parse_mode(c.format.map(parse_mode_from_format))
                .build()
        } else {
            InputMediaDocument::builder().media(upload).build()
        };
        media.push(MediaGroupInputMedia::Document(document));
    }

    let params = SendMediaGroupParams::builder()
        .chat_id(chat_id)
        .media(media)
        .maybe_reply_parameters(reply_parameters(reply_to))
        .build();
    let result = send_with_429_retry!(bot, send_media_group, &params);
    for c in &cleanups {
        cleanup_outbox(Some(c.as_path()));
    }
    match result {
        Ok(resp) => resp
            .result
            .into_iter()
            .map(|sent| {
                schedule_delete_after(bot, chat_id, sent.message_id, delete_after);
                sent.message_id
            })
            .collect(),
        Err(e) => {
            log::error!("TGAdapter send_media_group error: {e}");
            Vec::new()
        }
    }
}

fn emit_message_sent_if_needed(
    origin: &str,
    chat_id: i64,
    local_id: Option<&str>,
    message_id: Option<i32>,
) {
    let (Some(local_id), Some(message_id)) = (local_id, message_id) else {
        return;
    };

    let mut event = Event::message_sent(
        "TGAdapter",
        message_id.to_string(),
        Some(local_id.to_string()),
    );
    if let Some(msg) = event.message.as_mut() {
        msg.chat = snb_core::event::Chat::new(chat_id.to_string());
    }
    event.target_plugin = Some(origin.to_string());
    context::bot().emit_event(event);
}

async fn edit_message_item(bot: Bot, chat_id: i64, msg: Message) {
    let Some(message_id) = msg.id.as_deref() else {
        log::error!("TGAdapter edit_message missing message.id");
        return;
    };
    let Ok(message_id) = message_id.parse::<i32>() else {
        log::error!("TGAdapter edit_message requires native Telegram message id: {message_id}");
        return;
    };

    let mut text = OutgoingText::default();
    for item in &msg.content {
        if let ContentItem::Text {
            text: item_text,
            format,
        } = item
        {
            text.push_text(item_text.clone(), *format);
        }
    }
    let Some(payload) = text.into_payload() else {
        log::error!("TGAdapter edit_message has no text content");
        return;
    };

    // All EditMessageTextParams fields are Option (10.x rich_message
    // alternative) — the compiler will NOT catch a missing chat_id/message_id/
    // text here, so keep all three setters together.
    let params = EditMessageTextParams::builder()
        .chat_id(chat_id)
        .message_id(message_id)
        .text(payload.text)
        .maybe_parse_mode(payload.format.map(parse_mode_from_format))
        .build();
    if let Err(e) = bot.edit_message_text(&params).await {
        // Telegram returns an error when the new text is identical to the old
        // one; that is harmless for status updates, so only warn.
        log::warn!("TGAdapter edit_message error: {e}");
    }
}

async fn delete_message_item(bot: Bot, chat_id: i64, msg: Message) {
    let Some(message_id) = msg.id else {
        log::error!("TGAdapter delete_message missing message.id");
        return;
    };

    let Ok(message_id) = message_id.parse::<i32>() else {
        log::error!("TGAdapter delete_message requires native Telegram message id: {message_id}");
        return;
    };

    let params = DeleteMessageParams::builder()
        .chat_id(chat_id)
        .message_id(message_id)
        .build();
    if let Err(e) = bot.delete_message(&params).await {
        log::error!("TGAdapter delete_message error: {e}");
    }
}

fn schedule_delete_after(bot: &Bot, chat_id: i64, message_id: i32, delay: Option<Duration>) {
    let Some(delay) = delay else {
        return;
    };

    let bot = bot.clone();
    snb_core::task::spawn(async move {
        tokio::time::sleep(delay).await;
        let params = DeleteMessageParams::builder()
            .chat_id(chat_id)
            .message_id(message_id)
            .build();
        if let Err(e) = bot.delete_message(&params).await {
            log::warn!("TGAdapter delete_after delete_message error: {e}");
        }
    });
}

fn merge_captions(
    first: Option<FormattedPayload>,
    second: Option<String>,
) -> Option<FormattedPayload> {
    match (first, second.filter(|s| !s.is_empty())) {
        (Some(first), Some(second)) if first.text == second => Some(first),
        (Some(mut first), Some(second)) => {
            first.text.push('\n');
            first.text.push_str(&second);
            first.format = None;
            Some(first)
        }
        (Some(first), None) => Some(first),
        (None, Some(second)) => Some(FormattedPayload {
            text: second,
            format: None,
        }),
        (None, None) => None,
    }
}

#[allow(deprecated)]
fn parse_mode_from_format(format: TextFormat) -> ParseMode {
    match format {
        // Legacy Markdown is deprecated upstream but still accepted; policy:
        // pass plugin-requested formats through unchanged.
        TextFormat::Markdown => ParseMode::Markdown,
        TextFormat::MarkdownV2 => ParseMode::MarkdownV2,
        TextFormat::Html => ParseMode::Html,
    }
}

fn split_caption(caption: Option<FormattedPayload>) -> (Option<String>, Option<ParseMode>) {
    match caption {
        Some(c) => {
            let mode = c.format.map(parse_mode_from_format);
            (Some(c.text), mode)
        }
        None => (None, None),
    }
}

fn reply_parameters(reply_to: Option<&str>) -> Option<ReplyParameters> {
    let msg_id = reply_to?.parse::<i32>().ok()?;
    Some(ReplyParameters::builder().message_id(msg_id).build())
}

/// Prepare a `FileUpload` for a file source. Returns the upload plus an
/// optional outbox path to delete after the send completes.
///
/// frankenstein has no in-memory upload and no file-name override (the
/// multipart name is always the path basename, fetched with a panicking
/// `.unwrap()` upstream) — so byte payloads and renames are materialized
/// under `data/<plugin>/outbox/<unique>/<desired_name>`.
fn file_upload_from_source(
    source: FileSource,
    file_name: Option<String>,
    file_id: Option<String>,
) -> anyhow::Result<(FileUpload, Option<PathBuf>)> {
    if let Some(fid) = non_empty(file_id) {
        return Ok((FileUpload::String(fid), None));
    }
    match source {
        FileSource::Id(fid) => Ok((FileUpload::String(fid), None)),
        // file_ids and HTTP URLs share FileUpload::String; Telegram
        // disambiguates server-side.
        FileSource::Url(url) => Ok((FileUpload::String(url), None)),
        FileSource::Path(path) => {
            let path = PathBuf::from(path);
            let Some(basename) = path.file_name().and_then(|n| n.to_str()) else {
                anyhow::bail!("file path has no usable basename: {}", path.display());
            };
            match non_empty(file_name) {
                Some(name) if name != basename => {
                    let out = outbox_copy(&path, &name)?;
                    Ok((FileUpload::from(out.clone()), Some(out)))
                }
                _ => Ok((FileUpload::from(path), None)),
            }
        }
    }
}

fn file_upload_from_image(
    source: ImageSource,
    file_id: Option<String>,
) -> anyhow::Result<(FileUpload, Option<PathBuf>)> {
    if let Some(fid) = non_empty(file_id) {
        return Ok((FileUpload::String(fid), None));
    }
    match source {
        ImageSource::Id(fid) => Ok((FileUpload::String(fid), None)),
        ImageSource::Url(url) => Ok((FileUpload::String(url), None)),
        ImageSource::Path(path) => {
            let path = PathBuf::from(path);
            anyhow::ensure!(
                path.file_name().is_some(),
                "image path has no usable basename: {}",
                path.display()
            );
            Ok((FileUpload::from(path), None))
        }
        ImageSource::Base64(data) => {
            let name = format!("image.{}", base64_image_extension(&data));
            let bytes = decode_base64_image(&data)?;
            let out = outbox_write(&name, &bytes)?;
            Ok((FileUpload::from(out.clone()), Some(out)))
        }
    }
}

/// Best-effort extension from a data-URI prefix so Telegram's content-type
/// detection sees something sensible; photos are re-encoded server-side anyway.
fn base64_image_extension(data: &str) -> &'static str {
    let prefix = data.split_once(',').map_or("", |(p, _)| p);
    if prefix.contains("image/jpeg") || prefix.contains("image/jpg") {
        "jpg"
    } else if prefix.contains("image/webp") {
        "webp"
    } else if prefix.contains("image/gif") {
        "gif"
    } else {
        "png"
    }
}

static OUTBOX_SEQ: AtomicU64 = AtomicU64::new(0);

/// A unique per-send subdirectory under the plugin's confined data namespace
/// (never OS temp — outbox content is user data). The desired basename is
/// preserved inside it because the basename is what Telegram displays.
fn outbox_slot() -> anyhow::Result<PathBuf> {
    let dir = context::plugin()
        .data_dir()
        .join("outbox")
        .join(format!(
            "{}-{}",
            std::process::id(),
            OUTBOX_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("cannot create outbox dir {}", dir.display()))?;
    Ok(dir)
}

fn outbox_write(name: &str, bytes: &[u8]) -> anyhow::Result<PathBuf> {
    let path = outbox_slot()?.join(name);
    std::fs::write(&path, bytes)
        .with_context(|| format!("cannot write outbox file {}", path.display()))?;
    Ok(path)
}

fn outbox_copy(src: &Path, name: &str) -> anyhow::Result<PathBuf> {
    let path = outbox_slot()?.join(name);
    std::fs::copy(src, &path)
        .with_context(|| format!("cannot copy {} into outbox", src.display()))?;
    Ok(path)
}

/// Remove an outbox file together with its unique slot directory. Best-effort:
/// anything left behind is swept by the startup cleanup in `on_load`.
fn cleanup_outbox(path: Option<&Path>) {
    let Some(path) = path else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::remove_dir_all(dir);
    }
}

fn decode_base64_image(data: &str) -> anyhow::Result<Vec<u8>> {
    let encoded = data
        .split_once(',')
        .filter(|(prefix, _)| prefix.contains("base64"))
        .map_or(data, |(_, encoded)| encoded)
        .trim();
    general_purpose::STANDARD
        .decode(encoded)
        .context("invalid base64 image data")
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.is_empty())
}

#[cfg(test)]
#[path = "../tests/unit/outgoing_tests.rs"]
mod outgoing_tests;
