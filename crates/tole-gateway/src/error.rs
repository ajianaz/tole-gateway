//! Error types. Messages never carry secrets: no tokens, no URLs that embed a
//! token (the Telegram Bot API puts the bot token in the request path), no
//! request bodies.

use thiserror::Error;

/// A failure talking to `tole serve`.
#[derive(Debug, Error)]
pub enum ToleError {
    /// 401: the credential was refused.
    #[error("tole rejected the credentials")]
    Unauthorized,
    /// 404: unknown session or approval id.
    #[error("not found: {0}")]
    NotFound(String),
    /// 409 on a prompt: the session is already running a turn.
    #[error("the session is busy with a turn")]
    Busy,
    /// Any other 409 (writer lock held elsewhere, profile mismatch, and so on).
    #[error("conflict: {0}")]
    Conflict(String),
    /// 503: the server is at its live-session cap.
    #[error("tole is at capacity: {0}")]
    Unavailable(String),
    /// 400 or 403: the request was refused as malformed or not allowed.
    #[error("tole refused the request: {0}")]
    BadRequest(String),
    /// 5xx other than 503.
    #[error("tole server error: {0}")]
    Server(String),
    /// The connection failed (refused, reset, DNS).
    #[error("cannot reach tole: {0}")]
    Transport(String),
    /// The answer did not match the documented wire format.
    #[error("unexpected response from tole: {0}")]
    Decode(String),
    /// No answer within the configured timeout.
    #[error("tole did not answer in time")]
    Timeout,
}

impl ToleError {
    /// True for failures that a retry with backoff can plausibly fix.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            ToleError::Unavailable(_)
                | ToleError::Server(_)
                | ToleError::Transport(_)
                | ToleError::Timeout
        )
    }
}

/// A failure talking to a chat platform.
#[derive(Debug, Error)]
pub enum ChatError {
    /// The platform answered `ok: false` or an HTTP error status.
    #[error("chat platform rejected the request: {0}")]
    Api(String),
    /// The platform asked the client to slow down.
    #[error("rate limited, retry after {retry_after_secs}s")]
    RateLimited {
        /// Seconds the platform asked us to wait.
        retry_after_secs: u64,
    },
    /// The user blocked the bot or the chat no longer exists.
    #[error("the chat is not reachable (blocked or gone)")]
    Gone,
    /// The connection failed.
    #[error("cannot reach the chat platform: {0}")]
    Transport(String),
    /// The answer did not match the documented wire format.
    #[error("unexpected response from the chat platform: {0}")]
    Decode(String),
}

impl ChatError {
    /// True for failures that a retry with backoff can plausibly fix.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            ChatError::RateLimited { .. } | ChatError::Transport(_)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retryable_classification() {
        assert!(ToleError::Timeout.is_retryable());
        assert!(ToleError::Transport("x".into()).is_retryable());
        assert!(!ToleError::Unauthorized.is_retryable());
        assert!(!ToleError::Busy.is_retryable());
        assert!(ChatError::RateLimited {
            retry_after_secs: 3
        }
        .is_retryable());
        assert!(!ChatError::Gone.is_retryable());
    }
}
