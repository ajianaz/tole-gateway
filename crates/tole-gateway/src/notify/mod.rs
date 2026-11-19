//! Receiver for tole's signed outbound webhooks (`[webhook_out.<name>]`).
//!
//! **Off by default.** When `[notify] enabled = true` the gateway binds one
//! small HTTP listener (loopback unless explicitly told otherwise) and accepts
//! `POST <path>` from tole. It is a notification channel only: it carries no
//! credential and grants nothing; an approval is still answered through
//! `POST /approvals/{id}/decision`, and polling stays as the fallback.
//!
//! Every request goes through the same gate, in this order, and nothing in it
//! is trusted before the signature check passes:
//!
//! 1. method and path (else `404`), a concurrency cap (`503`);
//! 2. the body, read with a hard size cap and a deadline (`413` / `408`); a
//!    request that says it is a `turn_finished` gets the tighter
//!    [`MAX_TURN_BODY`] cap (a lie about the event fails the signature anyway);
//! 3. the four `X-Tole-*` headers, the v2 HMAC over the raw body compared in
//!    constant time, and the 300 second timestamp window: any failure is the
//!    same bare `401`, so the answer never says which check failed;
//! 4. only then the body is parsed, and its `v`, `id`, `type` and
//!    `destination` must agree with the headers and the configuration;
//! 5. dedupe on `(destination, id)`: tole retries at least once with the same
//!    id, so a repeat is answered `200` and applied no second time.
//!
//! The signing key is a [`Secret`]; neither it, a signature nor a header value
//! is ever logged or put in an answer. There is no TLS here: see
//! `docs/notify.md` for the reverse proxy or private network it is meant for.

mod hub;
mod turn;
mod verify;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Router;
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tracing::{debug, info};

pub use hub::{NotifyHub, Requested};
pub use turn::{TurnCode, TurnFinished, TurnStatus, MAX_TURN_BODY};
pub use verify::{sign, verify, Headers, Reject, WINDOW_SECS};

use crate::secret::Secret;

/// Receiver settings (from `[notify]` and the resolved key).
#[derive(Debug, Clone)]
pub struct ReceiverConfig {
    /// The HMAC signing key shared with the tole destination.
    pub key: Secret,
    /// URL path accepted.
    pub path: String,
    /// The tole destination name a body must carry.
    pub destination: String,
    /// Largest body accepted, in bytes.
    pub max_body_bytes: usize,
}

/// How long delivery ids are remembered (tole's retry horizon is shorter).
pub const SEEN_TTL_SECS: u64 = 24 * 3600;
/// Most delivery ids remembered; a full store refuses new events (`503`) so
/// tole retries, rather than forgetting one and applying it twice.
pub const SEEN_CAPACITY: usize = 10_000;
/// Longest a request may take to arrive.
const READ_DEADLINE: Duration = Duration::from_secs(10);
/// Requests handled at once.
const MAX_IN_FLIGHT: usize = 16;
/// Longest text field kept from an event.
const MAX_FIELD: usize = 512;

type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;

struct Shared {
    cfg: ReceiverConfig,
    hub: Arc<NotifyHub>,
    seen: Mutex<HashMap<String, u64>>,
    slots: Semaphore,
    now: Clock,
}

/// The receiver: build it, then [`Receiver::router`] or [`Receiver::serve`].
#[derive(Clone)]
pub struct Receiver(Arc<Shared>);

fn system_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Receiver {
    /// A receiver on the system clock.
    pub fn new(cfg: ReceiverConfig, hub: Arc<NotifyHub>) -> Self {
        Self::with_clock(cfg, hub, Arc::new(system_secs))
    }

    /// A receiver with an injected clock (unix seconds), for tests.
    pub fn with_clock(cfg: ReceiverConfig, hub: Arc<NotifyHub>, now: Clock) -> Self {
        Self(Arc::new(Shared {
            cfg,
            hub,
            seen: Mutex::new(HashMap::new()),
            slots: Semaphore::new(MAX_IN_FLIGHT),
            now,
        }))
    }

    /// The HTTP service. Every route but the configured path is a `404`.
    pub fn router(&self) -> Router {
        Router::new()
            .fallback(handle)
            .with_state(Arc::clone(&self.0))
    }

    /// Serve on `listener` until `shutdown` completes.
    pub async fn serve(
        self,
        listener: TcpListener,
        shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> std::io::Result<()> {
        axum::serve(listener, self.router())
            .with_graceful_shutdown(shutdown)
            .await
    }
}

/// Bind the receiver's address.
pub async fn bind(addr: SocketAddr) -> std::io::Result<TcpListener> {
    let l = TcpListener::bind(addr).await?;
    info!("tole webhook receiver listening on {}", l.local_addr()?);
    Ok(l)
}

enum Seen {
    New,
    Duplicate,
    Full,
}

impl Shared {
    /// Record `key` once; a second call inside the TTL is a duplicate.
    fn first_time(&self, key: String, now: u64) -> Seen {
        let mut seen = self.seen.lock().unwrap_or_else(|p| p.into_inner());
        if seen.get(&key).is_some_and(|until| *until > now) {
            return Seen::Duplicate;
        }
        if seen.len() >= SEEN_CAPACITY {
            seen.retain(|_, until| *until > now);
            if seen.len() >= SEEN_CAPACITY {
                return Seen::Full;
            }
        }
        seen.insert(key, now + SEEN_TTL_SECS);
        Seen::New
    }
}

fn status(code: StatusCode) -> Response {
    // Always an empty body: nothing to leak, nothing to tell.
    (code, Body::empty()).into_response()
}

fn header<'a>(h: &'a HeaderMap, name: &str) -> &'a str {
    h.get(name).and_then(|v| v.to_str().ok()).unwrap_or("")
}

async fn handle(State(s): State<Arc<Shared>>, req: Request) -> Response {
    if req.method() != Method::POST || req.uri().path() != s.cfg.path {
        return status(StatusCode::NOT_FOUND);
    }
    let Ok(_slot) = s.slots.try_acquire() else {
        return status(StatusCode::SERVICE_UNAVAILABLE);
    };
    let (parts, body) = req.into_parts();
    // The header is not trusted yet; it can only make the cap tighter.
    let limit = if header(&parts.headers, "x-tole-event") == "turn_finished" {
        s.cfg.max_body_bytes.min(MAX_TURN_BODY)
    } else {
        s.cfg.max_body_bytes
    };
    let declared = header(&parts.headers, "content-length")
        .parse::<usize>()
        .ok();
    if declared.is_some_and(|n| n > limit) {
        return status(StatusCode::PAYLOAD_TOO_LARGE);
    }
    let bytes = match tokio::time::timeout(READ_DEADLINE, axum::body::to_bytes(body, limit)).await {
        Ok(Ok(b)) => b,
        Ok(Err(_)) => return status(StatusCode::PAYLOAD_TOO_LARGE),
        Err(_) => return status(StatusCode::REQUEST_TIMEOUT),
    };

    let now = (s.now)();
    let hdrs = Headers {
        timestamp: header(&parts.headers, "x-tole-timestamp"),
        id: header(&parts.headers, "x-tole-delivery"),
        event: header(&parts.headers, "x-tole-event"),
        signature: header(&parts.headers, "x-tole-signature"),
    };
    if let Err(reason) = verify(&s.cfg.key, &hdrs, &bytes, now) {
        // The reason is kept for the operator at debug level; no received
        // value, signature or key is part of it, and the peer is told nothing.
        debug!("webhook delivery refused: {reason:?}");
        return status(StatusCode::UNAUTHORIZED);
    }

    // Authenticated from here on; the body is still checked, not trusted.
    let Ok(doc) = serde_json::from_slice::<Value>(&bytes) else {
        return status(StatusCode::BAD_REQUEST);
    };
    let consistent = doc["v"] == 1
        && doc["id"] == hdrs.id
        && doc["type"] == hdrs.event
        && doc["destination"] == s.cfg.destination.as_str();
    if !consistent {
        debug!("authenticated webhook body disagrees with its headers or this destination");
        return status(StatusCode::BAD_REQUEST);
    }
    let action = match parse_event(hdrs.event, hdrs.id, &doc) {
        Ok(a) => a,
        Err(()) => return status(StatusCode::BAD_REQUEST),
    };
    match s.first_time(format!("{}\n{}", s.cfg.destination, hdrs.id), now) {
        Seen::Duplicate => {
            debug!("duplicate webhook delivery acknowledged, not applied again");
            return status(StatusCode::OK);
        }
        Seen::Full => return status(StatusCode::SERVICE_UNAVAILABLE),
        Seen::New => {}
    }
    match action {
        Action::Requested(r) => s.hub.record_requested(r),
        Action::Resolved(id) => s.hub.record_resolved(&id),
        Action::Turn(t) => s.hub.record_turn(t),
        Action::Ignored => {}
    }
    debug!("webhook event accepted: {}", hdrs.event);
    status(StatusCode::OK)
}

enum Action {
    Requested(Requested),
    Resolved(String),
    Turn(TurnFinished),
    /// A well-formed event the gateway does not act on (`cron_run_finished`,
    /// `webhook_test`, future types): acknowledged so tole stops retrying.
    Ignored,
}

fn text(d: &Value, key: &str) -> Option<String> {
    d.get(key)?
        .as_str()
        .map(|s| s.chars().take(MAX_FIELD).collect())
}

fn id_field(d: &Value, key: &str) -> Option<String> {
    text(d, key).filter(|s| verify::valid_token(s, 128))
}

fn parse_event(event: &str, id: &str, doc: &Value) -> Result<Action, ()> {
    let data = &doc["data"];
    match event {
        "approval_requested" => Ok(Action::Requested(Requested {
            approval_id: id_field(data, "approval_id").ok_or(())?,
            session_id: text(data, "session_id")
                .filter(|s| !s.is_empty())
                .ok_or(())?,
            tool: text(data, "tool").filter(|s| !s.is_empty()).ok_or(())?,
            summary: text(data, "summary").unwrap_or_default(),
            created_at: data.get("created_at").and_then(Value::as_u64).unwrap_or(0),
            expires_at: data.get("expires_at").and_then(Value::as_u64).unwrap_or(0),
        })),
        "approval_resolved" => Ok(Action::Resolved(id_field(data, "approval_id").ok_or(())?)),
        "turn_finished" => {
            let at = doc.get("at").and_then(Value::as_u64).ok_or(())?;
            Ok(Action::Turn(turn::parse(id, at, data).ok_or(())?))
        }
        _ => Ok(Action::Ignored),
    }
}
