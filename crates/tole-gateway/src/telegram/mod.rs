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

use std::time::Duration;

use crate::error::ChatError;
use crate::secret::Secret;

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

/// The Telegram adapter (to be implemented by task W1-C).
#[derive(Debug, Clone)]
pub struct TelegramAdapter {
    _config: TelegramConfig,
}

impl TelegramAdapter {
    /// Build the adapter. Task W1-C replaces this body and implements
    /// [`crate::channel::ChatSource`] and [`crate::channel::ChatPort`].
    pub fn new(config: TelegramConfig) -> Result<Self, ChatError> {
        Ok(Self { _config: config })
    }
}
