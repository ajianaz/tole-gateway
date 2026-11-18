//! Telegram adapter (task W1-C).
//!
//! Contract:
//!
//! - A thin Bot API client over HTTPS (`{api_base}/bot<token>/<method>`):
//!   `getUpdates` (long poll with `timeout` and `offset`, `allowed_updates`
//!   limited to `message` and `callback_query`), `sendMessage`,
//!   `editMessageText`, `answerCallbackQuery`, `sendChatAction`,
//!   `setMyCommands`.
//! - Implements [`crate::channel::ChatSource`] (offset kept inside, advanced
//!   only past updates that were returned) and [`crate::channel::ChatPort`].
//! - Text is plain from the app's point of view. Pick one safe rendering
//!   (plain text, or MarkdownV2 with a tested escape function) and split on
//!   the 4096 character limit at line or word boundaries.
//! - Callback data is capped at 64 bytes by Telegram; the app supplies short
//!   data, the adapter must reject longer data instead of truncating it.
//! - The bot token is in the request path: no error, log line or `Debug`
//!   output may contain the URL or the token (strip the URL from transport
//!   errors). Group chats, channels and non-text updates are reported only as
//!   far as the app needs (private chats, text, callbacks).
//! - `429` maps to `ChatError::RateLimited { retry_after_secs }`; `403` (bot
//!   blocked) maps to `ChatError::Gone`.
//! - Tests run against a fake Bot API server on loopback; the sandbox has no
//!   route to api.telegram.org.

mod api;
mod model;
mod render;

use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

use crate::channel::{ApprovalPrompt, ChatId, ChatPort, ChatSource, Inbound, MessageRef};
use crate::error::ChatError;
use crate::secret::Secret;

use api::Api;

/// Telegram's cap on `callback_data`, in bytes.
const MAX_CALLBACK_DATA: usize = 64;
/// Telegram's cap on the `answerCallbackQuery` notice, in characters.
const MAX_ANSWER_TEXT: usize = 200;

/// Everything [`TelegramAdapter`] needs.
#[derive(Debug, Clone)]
pub struct TelegramConfig {
    /// Bot API base URL, `https://api.telegram.org` outside tests.
    pub api_base: String,
    /// The bot token.
    pub bot_token: Secret,
    /// Long-poll timeout.
    pub poll_timeout: Duration,
}

/// The Telegram adapter: a Bot API client that is both the inbound
/// [`ChatSource`] and the outbound [`ChatPort`]. Cloning shares the update
/// offset.
#[derive(Clone)]
pub struct TelegramAdapter {
    api: Api,
    /// Next `offset` for `getUpdates`; `None` before the first update.
    offset: Arc<Mutex<Option<i64>>>,
}

// Hand-written so nothing that could hold the token is ever printed.
impl fmt::Debug for TelegramAdapter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TelegramAdapter { .. }")
    }
}

impl TelegramAdapter {
    /// Build the adapter. Calls to a loopback `api_base` bypass any proxy
    /// from the environment; every other host keeps the default behavior.
    pub fn new(config: TelegramConfig) -> Result<Self, ChatError> {
        let api = Api::new(&config.api_base, config.bot_token, config.poll_timeout)?;
        Ok(Self {
            api,
            offset: Arc::new(Mutex::new(None)),
        })
    }

    /// Publish the bot's command menu (`new`, `cancel`, `status`, `help`).
    pub async fn register_commands(&self) -> Result<(), ChatError> {
        let body = json!({"commands": [
            {"command": "new", "description": "Start a new session"},
            {"command": "cancel", "description": "Cancel the running turn"},
            {"command": "status", "description": "Show the session status"},
            {"command": "help", "description": "Show help"},
        ]});
        self.api
            .call("setMyCommands", &body, None)
            .await
            .map(|_| ())
    }

    fn current_offset(&self) -> Option<i64> {
        *self.offset.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn advance_offset(&self, next: i64) {
        let mut g = self.offset.lock().unwrap_or_else(|p| p.into_inner());
        if g.is_none_or(|cur| next > cur) {
            *g = Some(next);
        }
    }

    async fn send_one(
        &self,
        chat: ChatId,
        text: &str,
        keyboard: Option<Value>,
    ) -> Result<MessageRef, ChatError> {
        let mut body = json!({
            "chat_id": chat.0,
            "text": text,
            "link_preview_options": {"is_disabled": true},
        });
        if let Some(k) = keyboard {
            body["reply_markup"] = k;
        }
        let result = self.api.call("sendMessage", &body, None).await?;
        let message_id = result
            .get("message_id")
            .and_then(Value::as_i64)
            .ok_or_else(|| ChatError::Decode("sendMessage result has no message_id".into()))?;
        Ok(MessageRef { chat, message_id })
    }

    /// Send every part; the keyboard (if any) goes on the last one. Returns
    /// the reference of the last part.
    async fn send_parts(
        &self,
        chat: ChatId,
        text: &str,
        keyboard: Option<Value>,
    ) -> Result<MessageRef, ChatError> {
        let parts = split_or_placeholder(text);
        let last = parts.len() - 1;
        let mut keyboard = keyboard;
        let mut sent = None;
        for (i, part) in parts.iter().enumerate() {
            let k = if i == last { keyboard.take() } else { None };
            sent = Some(self.send_one(chat, part, k).await?);
        }
        sent.ok_or_else(|| ChatError::Decode("nothing was sent".into()))
    }
}

fn split_or_placeholder(text: &str) -> Vec<String> {
    let parts = render::split_message(text);
    if parts.is_empty() {
        vec!["(empty message)".to_string()]
    } else {
        parts
    }
}

fn keyboard(prompt: &ApprovalPrompt) -> Result<Value, ChatError> {
    let mut rows = Vec::new();
    for row in &prompt.buttons {
        let mut out = Vec::new();
        for b in row {
            if b.data.is_empty() || b.data.len() > MAX_CALLBACK_DATA {
                return Err(ChatError::Api(format!(
                    "button data must be 1 to {MAX_CALLBACK_DATA} bytes, got {}",
                    b.data.len()
                )));
            }
            out.push(json!({"text": b.label, "callback_data": b.data}));
        }
        rows.push(out);
    }
    Ok(json!({"inline_keyboard": rows}))
}

impl ChatSource for TelegramAdapter {
    async fn next(&self) -> Result<Vec<Inbound>, ChatError> {
        let raw = self.api.get_updates(self.current_offset()).await?;
        let inbound: Vec<Inbound> = raw.iter().filter_map(model::normalize).collect();
        // No await between here and the return: if the caller drops the
        // future earlier, the offset is untouched and Telegram redelivers.
        if let Some(next) = model::next_offset(&raw) {
            self.advance_offset(next);
        }
        Ok(inbound)
    }
}

impl ChatPort for TelegramAdapter {
    async fn send_text(&self, chat: ChatId, text: &str) -> Result<MessageRef, ChatError> {
        self.send_parts(chat, text, None).await
    }

    async fn send_approval(
        &self,
        chat: ChatId,
        prompt: &ApprovalPrompt,
    ) -> Result<MessageRef, ChatError> {
        let kb = keyboard(prompt)?;
        self.send_parts(chat, &prompt.text, Some(kb)).await
    }

    async fn finish_approval(&self, message: MessageRef, text: &str) -> Result<(), ChatError> {
        // An edit is one message: keep the first part only.
        let text = split_or_placeholder(text).swap_remove(0);
        let body = json!({
            "chat_id": message.chat.0,
            "message_id": message.message_id,
            "text": text,
            "link_preview_options": {"is_disabled": true},
            "reply_markup": {"inline_keyboard": []},
        });
        match self.api.call("editMessageText", &body, None).await {
            Ok(_) => Ok(()),
            // Editing to identical text is a success for our purposes.
            Err(ChatError::Api(m)) if m.contains("message is not modified") => Ok(()),
            Err(e) => Err(e),
        }
    }

    async fn typing(&self, chat: ChatId) -> Result<(), ChatError> {
        let body = json!({"chat_id": chat.0, "action": "typing"});
        self.api
            .call("sendChatAction", &body, None)
            .await
            .map(|_| ())
    }

    async fn answer_callback(
        &self,
        callback_id: &str,
        text: Option<&str>,
    ) -> Result<(), ChatError> {
        let mut body = json!({"callback_query_id": callback_id});
        if let Some(t) = text {
            body["text"] = Value::from(t.chars().take(MAX_ANSWER_TEXT).collect::<String>());
        }
        self.api
            .call("answerCallbackQuery", &body, None)
            .await
            .map(|_| ())
    }
}
