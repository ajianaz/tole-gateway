//! HTTP client for the tole serve API (task W1-B).
//!
//! Contract (from `docs/mobile-control.md` in the tole repository, verified
//! against `crates/tole-cli/src/serve.rs` on develop):
//!
//! - Every route except `GET /health` needs `Authorization: Bearer <token>` or
//!   HTTP Basic (see [`crate::config::ToleAuth`]).
//! - The server speaks one request per connection with `Connection: close`.
//! - `POST /sessions` `{cwd?, profile?}` creates; `{resume, profile?, cwd?}`
//!   resumes. Answer: [`crate::types::SessionOpen`].
//! - `POST /sessions/{id}/prompt` `{text}` blocks for the whole turn (use the
//!   long prompt timeout). `200 {"stopReason": "...", "text"?: "..."}`, or
//!   `200 {"stopReason":"approval_required","approval"?: {row}}`. `409` means
//!   the session is already running a turn ([`crate::error::ToleError::Busy`]).
//! - `POST /sessions/{id}/cancel`, `GET /sessions/{id}/status`,
//!   `GET /approvals?session=<id>`, `POST /approvals/{id}/decision`
//!   `{"decision":"allow"|"deny"}` (`202 {resuming:true}` for allow, `200` for
//!   deny, `404` unknown id), `GET /sessions/{id}/entries?after=&limit=`.
//! - After an allow, the resumed turn has NO HTTP answer: the caller polls
//!   status and entries (that logic lives in the app, not here).
//! - Errors map to [`crate::error::ToleError`]. Never put the token in an error.

use std::future::Future;
use std::time::Duration;

use crate::config::ToleAuth;
use crate::error::ToleError;
use crate::types::{
    ApprovalRow, Decision, DecisionAck, EntriesPage, PromptOutcome, SessionOpen, SessionStatus,
};

/// The operations the gateway needs from `tole serve`. The app is generic over
/// this trait so it can be tested against a fake.
pub trait ToleApi: Send + Sync {
    /// `GET /health`.
    fn health(&self) -> impl Future<Output = Result<(), ToleError>> + Send;

    /// `POST /sessions` with an optional profile and working directory.
    fn create_session(
        &self,
        profile: Option<&str>,
        cwd: Option<&str>,
    ) -> impl Future<Output = Result<SessionOpen, ToleError>> + Send;

    /// `POST /sessions` `{resume}` with an optional profile.
    fn resume_session(
        &self,
        session_id: &str,
        profile: Option<&str>,
    ) -> impl Future<Output = Result<SessionOpen, ToleError>> + Send;

    /// `POST /sessions/{id}/prompt`. Blocks for the whole turn.
    fn prompt(
        &self,
        session_id: &str,
        text: &str,
    ) -> impl Future<Output = Result<PromptOutcome, ToleError>> + Send;

    /// `POST /sessions/{id}/cancel`.
    fn cancel(&self, session_id: &str) -> impl Future<Output = Result<(), ToleError>> + Send;

    /// `GET /sessions/{id}/status`.
    fn status(
        &self,
        session_id: &str,
    ) -> impl Future<Output = Result<SessionStatus, ToleError>> + Send;

    /// `GET /approvals`, filtered to one session when given.
    fn approvals(
        &self,
        session_id: Option<&str>,
    ) -> impl Future<Output = Result<Vec<ApprovalRow>, ToleError>> + Send;

    /// `POST /approvals/{id}/decision`.
    fn decide(
        &self,
        approval_id: &str,
        decision: Decision,
    ) -> impl Future<Output = Result<DecisionAck, ToleError>> + Send;

    /// `GET /sessions/{id}/entries?after=&limit=`.
    fn entries(
        &self,
        session_id: &str,
        after: u64,
        limit: u32,
    ) -> impl Future<Output = Result<EntriesPage, ToleError>> + Send;
}

/// Everything [`HttpToleClient`] needs.
#[derive(Debug, Clone)]
pub struct ToleClientConfig {
    /// Base URL, for example `http://127.0.0.1:7801`.
    pub base_url: String,
    /// The credential.
    pub auth: ToleAuth,
    /// Timeout for ordinary requests.
    pub request_timeout: Duration,
    /// Timeout for `prompt`.
    pub prompt_timeout: Duration,
}

/// [`ToleApi`] over HTTP (to be implemented by task W1-B).
#[derive(Debug, Clone)]
pub struct HttpToleClient {
    _config: ToleClientConfig,
}

impl HttpToleClient {
    /// Build a client. Task W1-B replaces this body with the real constructor
    /// (HTTP client setup) and implements [`ToleApi`].
    pub fn new(config: ToleClientConfig) -> Result<Self, ToleError> {
        Ok(Self { _config: config })
    }
}
