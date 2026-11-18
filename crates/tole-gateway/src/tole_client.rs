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

use std::fmt;
use std::future::Future;
use std::time::Duration;

use reqwest::{Method, StatusCode, Url};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

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

/// Longest server error text kept in a [`ToleError`], in characters.
const ERROR_TEXT_CAP: usize = 300;

/// How long a TCP connect may take. Tole runs on the same host, so this is short.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// [`ToleApi`] over HTTP with `reqwest`.
///
/// The client never uses a proxy (tole runs on the same host), never follows
/// redirects (so the credential cannot be forwarded elsewhere) and its `Debug`
/// output prints nothing about the credential or the address.
#[derive(Clone)]
pub struct HttpToleClient {
    http: reqwest::Client,
    base: Url,
    auth: ToleAuth,
    request_timeout: Duration,
    prompt_timeout: Duration,
}

impl fmt::Debug for HttpToleClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpToleClient").finish_non_exhaustive()
    }
}

/// A successful (2xx) answer.
struct Reply {
    body: Vec<u8>,
}

impl HttpToleClient {
    /// Build a client from the configuration.
    ///
    /// Fails with [`ToleError::BadRequest`] when the base URL is not an
    /// `http` or `https` URL, and [`ToleError::Transport`] when the HTTP
    /// stack cannot be set up.
    pub fn new(config: ToleClientConfig) -> Result<Self, ToleError> {
        let trimmed = config.base_url.trim().trim_end_matches('/');
        let mut base = Url::parse(trimmed)
            .map_err(|_| ToleError::BadRequest("the tole base URL is not a valid URL".into()))?;
        if !matches!(base.scheme(), "http" | "https") || base.host_str().is_none() {
            return Err(ToleError::BadRequest(
                "the tole base URL must be an http or https URL".into(),
            ));
        }
        // Credentials belong in the config, never in the URL.
        let _ = base.set_username("");
        let _ = base.set_password(None);
        base.set_query(None);
        base.set_fragment(None);
        let http = reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(CONNECT_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| ToleError::Transport(transport_text(e)))?;
        Ok(Self {
            http,
            base,
            auth: config.auth,
            request_timeout: config.request_timeout,
            prompt_timeout: config.prompt_timeout,
        })
    }

    /// The full URL for path `segments` (each percent-encoded) and `query`.
    fn url(&self, segments: &[&str], query: &[(&str, String)]) -> Url {
        let mut url = self.base.clone();
        if let Ok(mut path) = url.path_segments_mut() {
            path.pop_if_empty().extend(segments);
        }
        if !query.is_empty() {
            let mut pairs = url.query_pairs_mut();
            for (k, v) in query {
                pairs.append_pair(k, v);
            }
        }
        url
    }

    /// Send one request. Returns the 2xx answer or the mapped error.
    async fn send(
        &self,
        method: Method,
        url: Url,
        body: Option<Value>,
        timeout: Duration,
        busy_on_409: bool,
    ) -> Result<Reply, ToleError> {
        let mut req = self.http.request(method, url).timeout(timeout);
        req = match &self.auth {
            ToleAuth::Bearer(token) => req.bearer_auth(token.expose()),
            ToleAuth::Basic { user, password } => req.basic_auth(user, Some(password.expose())),
        };
        if let Some(body) = body {
            req = req.json(&body);
        }
        let resp = req.send().await.map_err(map_reqwest_error)?;
        let status = resp.status();
        let bytes = resp.bytes().await.map_err(map_reqwest_error)?;
        if status.is_success() {
            return Ok(Reply {
                body: bytes.to_vec(),
            });
        }
        Err(map_status(status, &bytes, busy_on_409))
    }

    /// Send and decode the JSON body.
    async fn json<T: DeserializeOwned>(
        &self,
        method: Method,
        url: Url,
        body: Option<Value>,
        timeout: Duration,
    ) -> Result<T, ToleError> {
        let reply = self.send(method, url, body, timeout, false).await?;
        decode(&reply.body)
    }
}

fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, ToleError> {
    serde_json::from_slice(bytes).map_err(|e| ToleError::Decode(cap(&e.to_string())))
}

/// Cut `text` to [`ERROR_TEXT_CAP`] characters.
fn cap(text: &str) -> String {
    let text = text.trim();
    match text.char_indices().nth(ERROR_TEXT_CAP) {
        Some((idx, _)) => format!("{}...", &text[..idx]),
        None => text.to_string(),
    }
}

/// The server's `{"error": "..."}` text, or the raw body, capped.
fn error_text(body: &[u8]) -> String {
    let text = match serde_json::from_slice::<Value>(body) {
        Ok(v) => match v.get("error").and_then(Value::as_str) {
            Some(s) => s.to_string(),
            None => String::from_utf8_lossy(body).into_owned(),
        },
        Err(_) => String::from_utf8_lossy(body).into_owned(),
    };
    cap(&text)
}

fn map_status(status: StatusCode, body: &[u8], busy_on_409: bool) -> ToleError {
    let code = status.as_u16();
    match code {
        401 => ToleError::Unauthorized,
        404 => ToleError::NotFound(error_text(body)),
        409 if busy_on_409 => ToleError::Busy,
        409 => ToleError::Conflict(error_text(body)),
        400 | 403 => ToleError::BadRequest(error_text(body)),
        503 => ToleError::Unavailable(error_text(body)),
        500..=599 => ToleError::Server(error_text(body)),
        400..=499 => ToleError::BadRequest(error_text(body)),
        _ => ToleError::Decode(format!("unexpected status {code}")),
    }
}

/// Describe a transport failure without the URL or anything credential-like.
fn transport_text(e: reqwest::Error) -> String {
    let e = e.without_url();
    if e.is_connect() {
        "connection failed (refused, reset or unresolved)".to_string()
    } else if e.is_builder() {
        "invalid request".to_string()
    } else {
        cap(&e.to_string())
    }
}

fn map_reqwest_error(e: reqwest::Error) -> ToleError {
    if e.is_timeout() {
        ToleError::Timeout
    } else if e.is_decode() {
        ToleError::Decode("the response body could not be read".into())
    } else {
        ToleError::Transport(transport_text(e))
    }
}

impl ToleApi for HttpToleClient {
    async fn health(&self) -> Result<(), ToleError> {
        let url = self.url(&["health"], &[]);
        self.send(Method::GET, url, None, self.request_timeout, false)
            .await
            .map(|_| ())
    }

    async fn create_session(
        &self,
        profile: Option<&str>,
        cwd: Option<&str>,
    ) -> Result<SessionOpen, ToleError> {
        let mut body = serde_json::Map::new();
        if let Some(cwd) = cwd {
            body.insert("cwd".into(), json!(cwd));
        }
        if let Some(profile) = profile {
            body.insert("profile".into(), json!(profile));
        }
        let url = self.url(&["sessions"], &[]);
        self.json(
            Method::POST,
            url,
            Some(Value::Object(body)),
            self.request_timeout,
        )
        .await
    }

    async fn resume_session(
        &self,
        session_id: &str,
        profile: Option<&str>,
    ) -> Result<SessionOpen, ToleError> {
        let mut body = serde_json::Map::new();
        body.insert("resume".into(), json!(session_id));
        if let Some(profile) = profile {
            body.insert("profile".into(), json!(profile));
        }
        let url = self.url(&["sessions"], &[]);
        self.json(
            Method::POST,
            url,
            Some(Value::Object(body)),
            self.request_timeout,
        )
        .await
    }

    async fn prompt(&self, session_id: &str, text: &str) -> Result<PromptOutcome, ToleError> {
        let url = self.url(&["sessions", session_id, "prompt"], &[]);
        let reply = self
            .send(
                Method::POST,
                url,
                Some(json!({ "text": text })),
                self.prompt_timeout,
                true,
            )
            .await?;
        let value: Value = decode(&reply.body)?;
        let stop_reason = value
            .get("stopReason")
            .and_then(Value::as_str)
            .ok_or_else(|| ToleError::Decode("the prompt answer has no stopReason".into()))?
            .to_string();
        if stop_reason == "approval_required" {
            let approval = match value.get("approval") {
                None | Some(Value::Null) => None,
                Some(row) => Some(
                    serde_json::from_value::<ApprovalRow>(row.clone())
                        .map_err(|e| ToleError::Decode(cap(&e.to_string())))?,
                ),
            };
            return Ok(PromptOutcome::ApprovalRequired { approval });
        }
        let text = value
            .get("text")
            .and_then(Value::as_str)
            .map(str::to_string);
        Ok(PromptOutcome::Reply { stop_reason, text })
    }

    async fn cancel(&self, session_id: &str) -> Result<(), ToleError> {
        let url = self.url(&["sessions", session_id, "cancel"], &[]);
        self.send(Method::POST, url, None, self.request_timeout, false)
            .await
            .map(|_| ())
    }

    async fn status(&self, session_id: &str) -> Result<SessionStatus, ToleError> {
        let url = self.url(&["sessions", session_id, "status"], &[]);
        self.json(Method::GET, url, None, self.request_timeout)
            .await
    }

    async fn approvals(&self, session_id: Option<&str>) -> Result<Vec<ApprovalRow>, ToleError> {
        #[derive(serde::Deserialize)]
        struct List {
            #[serde(default)]
            approvals: Vec<ApprovalRow>,
        }
        let query: Vec<(&str, String)> = session_id
            .map(|s| vec![("session", s.to_string())])
            .unwrap_or_default();
        let url = self.url(&["approvals"], &query);
        let list: List = self
            .json(Method::GET, url, None, self.request_timeout)
            .await?;
        Ok(list.approvals)
    }

    async fn decide(
        &self,
        approval_id: &str,
        decision: Decision,
    ) -> Result<DecisionAck, ToleError> {
        let url = self.url(&["approvals", approval_id, "decision"], &[]);
        self.json(
            Method::POST,
            url,
            Some(json!({ "decision": decision.as_str() })),
            self.request_timeout,
        )
        .await
    }

    async fn entries(
        &self,
        session_id: &str,
        after: u64,
        limit: u32,
    ) -> Result<EntriesPage, ToleError> {
        let query = [("after", after.to_string()), ("limit", limit.to_string())];
        let url = self.url(&["sessions", session_id, "entries"], &query);
        self.json(Method::GET, url, None, self.request_timeout)
            .await
    }
}
