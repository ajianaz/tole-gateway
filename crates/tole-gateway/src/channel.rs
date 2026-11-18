//! The chat-facing ports.
//!
//! [`ChatSource`] delivers inbound events and [`ChatPort`] sends outbound
//! messages. The Telegram adapter implements both; the app is generic over
//! them, so tests drive it with a fake channel and no network.
//!
//! Note: this is the seam for testing and for the first adapter. Per
//! `docs/design.md` the cross-adapter trait is extracted only once a second
//! adapter (Discord) exists, so keep these types free of Telegram specifics.

use std::future::Future;

use crate::error::ChatError;

/// A chat (for a private chat this equals the user id on Telegram).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChatId(pub i64);

/// A user on the chat platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct UserId(pub i64);

/// A sent message that can later be edited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessageRef {
    /// The chat the message is in.
    pub chat: ChatId,
    /// The platform's message id.
    pub message_id: i64,
}

/// One inline button.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Button {
    /// The visible label.
    pub label: String,
    /// Opaque data returned when the button is pressed. Platforms cap its size
    /// (Telegram: 64 bytes), so keep it short.
    pub data: String,
}

/// Text plus inline buttons (rows of buttons).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalPrompt {
    /// The message text (plain text; the adapter escapes it for the platform).
    pub text: String,
    /// Rows of buttons.
    pub buttons: Vec<Vec<Button>>,
}

/// Something that happened in a chat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inbound {
    /// A text message (a leading `/` marks a command).
    Message {
        /// The chat.
        chat: ChatId,
        /// The sender.
        user: UserId,
        /// The text.
        text: String,
        /// The platform message id.
        message_id: i64,
    },
    /// A press of an inline button.
    Callback {
        /// The chat of the message that carries the button.
        chat: ChatId,
        /// Who pressed it.
        user: UserId,
        /// Must be answered with [`ChatPort::answer_callback`].
        callback_id: String,
        /// The button's data.
        data: String,
        /// The message that carries the button, when known.
        message: Option<MessageRef>,
    },
}

/// A source of inbound events (long polling on Telegram).
pub trait ChatSource: Send + Sync {
    /// Wait for the next batch of events. An empty batch is normal (poll
    /// timeout). The adapter owns offsets and acknowledges what it returned.
    fn next(&self) -> impl Future<Output = Result<Vec<Inbound>, ChatError>> + Send;
}

/// Outbound messages. Text is plain; the adapter makes it safe for the platform
/// and splits it when it is longer than the platform allows.
pub trait ChatPort: Send + Sync {
    /// Send text; returns the reference of the last message sent.
    fn send_text(
        &self,
        chat: ChatId,
        text: &str,
    ) -> impl Future<Output = Result<MessageRef, ChatError>> + Send;

    /// Send text with inline buttons.
    fn send_approval(
        &self,
        chat: ChatId,
        prompt: &ApprovalPrompt,
    ) -> impl Future<Output = Result<MessageRef, ChatError>> + Send;

    /// Replace the text of a sent message and remove its buttons.
    fn finish_approval(
        &self,
        message: MessageRef,
        text: &str,
    ) -> impl Future<Output = Result<(), ChatError>> + Send;

    /// Show the typing indicator.
    fn typing(&self, chat: ChatId) -> impl Future<Output = Result<(), ChatError>> + Send;

    /// Acknowledge a button press (stops the client spinner); `text` is an
    /// optional short notice.
    fn answer_callback(
        &self,
        callback_id: &str,
        text: Option<&str>,
    ) -> impl Future<Output = Result<(), ChatError>> + Send;
}
