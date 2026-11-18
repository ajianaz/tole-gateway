//! A scriptable fake `tole serve`, for integration tests.
//!
//! Include it from a test file with
//! `#[path = "support/fake_tole.rs"] mod fake_tole;`. It needs only `axum`,
//! `tokio` and `serde_json`, so it compiles standalone.
//!
//! ```ignore
//! let fake = FakeTole::start().await;
//! fake.add_session("srv-1");
//! fake.script_prompt(PromptStep::reply("end_turn", Some("hello")));
//! // point the client at fake.url()
//! ```
//!
//! # API
//!
//! - `FakeTole::start().await` binds `127.0.0.1:0` and serves until dropped;
//!   `url()` is `http://127.0.0.1:<port>`.
//! - Auth: `require_bearer(token)` / `require_basic(user, password)` make every
//!   route except `GET /health` answer `401 {"error":"unauthorized"}` without a
//!   matching credential; `require_nothing()` turns it off again.
//! - Sessions: `add_session(id)`, `set_entries(id, entries)` (each entry is a
//!   JSON object; `seq` is filled in as 1, 2, ... when missing),
//!   `add_message(id, role, text)`. `POST /sessions` creates `srv-N` sessions;
//!   a resume of an unknown id is a 404. Unknown sessions answer 404. The real
//!   server (tole 0.11.0) answers a PROMPT to one with
//!   `500 {"error":"unknown session: <id>"}`; `real_unknown_prompt_500()`
//!   switches the fake to that, and `restart()` does it too.
//! - `restart()` simulates a `tole serve` restart: every session stops being
//!   live (all routes then answer as for an unknown session) but its log
//!   survives, and `POST /sessions {"resume": id}` makes it live again.
//! - Prompts: `script_prompt(PromptStep)` queues one answer per prompt call
//!   (FIFO); with an empty queue a prompt answers `end_turn` with `"ok"`.
//!   `PromptStep::delayed(duration, step)` sleeps before answering.
//! - Approvals: `add_approval(id, session, tool, description)` adds a pending
//!   row; `approval_row(id)` reads it back; deciding changes its status.
//!   `decisions()` lists `(approval id, "allow" | "deny")` in order.
//! - Failures: `fail(method, path, status, error_text)` makes that exact route
//!   (decoded path, no query) answer the given status and `{"error": text}`;
//!   `fail_with_body(...)` sends a raw body; `clear_failures()` removes them.
//! - Log: `requests()` returns every request as a `LoggedRequest` with the
//!   method, raw path and query, the auth scheme seen (never the secret
//!   value) and the JSON body. `last_request()`, `requests_to(path)`.

#![allow(dead_code)]

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Router;
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// What the fake does for one `POST /sessions/{id}/prompt`.
#[derive(Debug, Clone)]
pub enum PromptStep {
    /// `200 {"stopReason", "text"?}`.
    Reply {
        stop_reason: String,
        text: Option<String>,
    },
    /// `200 {"stopReason":"approval_required","approval"?: row}`.
    ApprovalRequired { approval: Option<Value> },
    /// Any status with `{"error": text}`.
    Error { status: u16, text: String },
    /// Sleep, then do the inner step.
    Delayed(Duration, Box<PromptStep>),
}

impl PromptStep {
    pub fn reply(stop_reason: &str, text: Option<&str>) -> Self {
        PromptStep::Reply {
            stop_reason: stop_reason.into(),
            text: text.map(str::to_string),
        }
    }

    pub fn approval_required(approval: Option<Value>) -> Self {
        PromptStep::ApprovalRequired { approval }
    }

    pub fn error(status: u16, text: &str) -> Self {
        PromptStep::Error {
            status,
            text: text.into(),
        }
    }

    pub fn delayed(delay: Duration, step: PromptStep) -> Self {
        PromptStep::Delayed(delay, Box::new(step))
    }
}

/// One request the fake saw. The credential value is never stored.
#[derive(Debug, Clone)]
pub struct LoggedRequest {
    pub method: String,
    /// The raw (still percent-encoded) path, without the query.
    pub path: String,
    /// The raw query string, without the `?`.
    pub query: String,
    /// `none`, `bearer`, `basic` or `other`.
    pub auth_scheme: String,
    /// True when the credential matched the one the fake requires (or none is required).
    pub auth_ok: bool,
    /// The parsed JSON body, `Null` when empty or not JSON.
    pub body: Value,
}

#[derive(Debug, Clone)]
enum Credential {
    Bearer(String),
    Basic(String, String),
}

#[derive(Default)]
struct Inner {
    credential: Option<Credential>,
    prompts: VecDeque<PromptStep>,
    approvals: Vec<Value>,
    sessions: HashMap<String, Vec<Value>>,
    /// Sessions whose log survived a `restart()` and are not live.
    dormant: HashMap<String, Vec<Value>>,
    /// A prompt to an unknown session answers the real server's 500.
    unknown_prompt_500: bool,
    failures: Vec<(String, String, u16, String)>,
    decisions: Vec<(String, String)>,
    log: Vec<LoggedRequest>,
    next_session: u64,
}

type Shared = Arc<Mutex<Inner>>;

/// The running fake server. Dropping it stops the server.
pub struct FakeTole {
    addr: std::net::SocketAddr,
    state: Shared,
    task: JoinHandle<()>,
}

impl Drop for FakeTole {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl FakeTole {
    /// Bind `127.0.0.1:0` and start serving.
    pub async fn start() -> FakeTole {
        let state: Shared = Arc::new(Mutex::new(Inner::default()));
        let app = Router::new().fallback(handle).with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind fake");
        let addr = listener.local_addr().expect("fake addr");
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        FakeTole { addr, state, task }
    }

    /// `http://127.0.0.1:<port>`.
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn with<R>(&self, f: impl FnOnce(&mut Inner) -> R) -> R {
        f(&mut self.state.lock().expect("fake state"))
    }

    pub fn require_bearer(&self, token: &str) {
        self.with(|s| s.credential = Some(Credential::Bearer(token.into())));
    }

    pub fn require_basic(&self, user: &str, password: &str) {
        self.with(|s| s.credential = Some(Credential::Basic(user.into(), password.into())));
    }

    pub fn require_nothing(&self) {
        self.with(|s| s.credential = None);
    }

    pub fn add_session(&self, id: &str) {
        self.with(|s| {
            s.sessions.entry(id.to_string()).or_default();
        });
    }

    pub fn set_entries(&self, id: &str, entries: Vec<Value>) {
        self.with(|s| {
            let entries = entries
                .into_iter()
                .enumerate()
                .map(|(i, mut e)| {
                    if e.get("seq").is_none() {
                        e["seq"] = json!(i as u64 + 1);
                    }
                    e
                })
                .collect();
            s.sessions.insert(id.to_string(), entries);
        });
    }

    /// Answer a prompt to an unknown session like the real server: `500`.
    pub fn real_unknown_prompt_500(&self) {
        self.with(|s| s.unknown_prompt_500 = true);
    }

    /// Simulate a `tole serve` restart: no session is live any more, the logs
    /// survive and can be resumed.
    pub fn restart(&self) {
        self.real_unknown_prompt_500();
        self.with(|s| {
            let live: Vec<_> = s.sessions.drain().collect();
            s.dormant.extend(live);
        });
    }

    /// Forget the logs that survived a `restart()` (the session files are gone).
    pub fn lose_dormant_logs(&self) {
        self.with(|s| s.dormant.clear());
    }

    pub fn add_message(&self, id: &str, role: &str, text: &str) {
        self.with(|s| {
            let log = s.sessions.entry(id.to_string()).or_default();
            let seq = log.last().and_then(|e| e["seq"].as_u64()).unwrap_or(0) + 1;
            log.push(json!({
                "seq": seq, "id": format!("e_{seq}"), "parentId": null,
                "type": "message", "timestamp": 1_787_807_227_226u64,
                "payload": {"role": role, "text": text}
            }));
        });
    }

    pub fn script_prompt(&self, step: PromptStep) {
        self.with(|s| s.prompts.push_back(step));
    }

    /// Add a pending approval and return its row.
    pub fn add_approval(&self, id: &str, session: &str, tool: &str, description: &str) -> Value {
        // Stamped with the current time, as tole does when it queues the row.
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let row = json!({
            "id": id, "sessionId": session, "tool": tool,
            "description": description, "status": "pending",
            "createdAtMs": now_ms, "expiresInSeconds": 900
        });
        self.with(|s| s.approvals.push(row.clone()));
        row
    }

    pub fn approval_row(&self, id: &str) -> Option<Value> {
        self.with(|s| s.approvals.iter().find(|a| a["id"] == id).cloned())
    }

    pub fn decisions(&self) -> Vec<(String, String)> {
        self.with(|s| s.decisions.clone())
    }

    /// Make `method path` (decoded path, no query) answer `status {"error": text}`.
    pub fn fail(&self, method: &str, path: &str, status: u16, error_text: &str) {
        self.fail_with_body(
            method,
            path,
            status,
            &json!({ "error": error_text }).to_string(),
        );
    }

    /// Like [`FakeTole::fail`] with a raw response body.
    pub fn fail_with_body(&self, method: &str, path: &str, status: u16, body: &str) {
        self.with(|s| {
            s.failures
                .push((method.into(), path.into(), status, body.into()))
        });
    }

    pub fn clear_failures(&self) {
        self.with(|s| s.failures.clear());
    }

    pub fn requests(&self) -> Vec<LoggedRequest> {
        self.with(|s| s.log.clone())
    }

    pub fn last_request(&self) -> Option<LoggedRequest> {
        self.with(|s| s.log.last().cloned())
    }

    /// Requests whose raw path equals `path`.
    pub fn requests_to(&self, path: &str) -> Vec<LoggedRequest> {
        self.with(|s| s.log.iter().filter(|r| r.path == path).cloned().collect())
    }
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn query_param(query: &str, name: &str) -> Option<String> {
    query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| percent_decode(&v.replace('+', " ")))
}

fn json_response(status: u16, body: Value) -> Response {
    raw_response(status, body.to_string())
}

fn raw_response(status: u16, body: String) -> Response {
    Response::builder()
        .status(StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .expect("response")
        .into_response()
}

fn b64_decode(s: &str) -> Option<String> {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut acc = 0u32;
    let mut bits = 0;
    let mut out = Vec::new();
    for c in s.trim_end_matches('=').bytes() {
        let v = T.iter().position(|&t| t == c)? as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    String::from_utf8(out).ok()
}

/// `(scheme, matches)` for the Authorization header against the requirement.
fn check_auth(cred: &Option<Credential>, header_value: Option<&str>) -> (String, bool) {
    let (scheme, ok) = match header_value {
        None => ("none", false),
        Some(h) => {
            if let Some(t) = h.strip_prefix("Bearer ") {
                (
                    "bearer",
                    matches!(cred, Some(Credential::Bearer(want)) if want == t),
                )
            } else if let Some(b) = h.strip_prefix("Basic ") {
                let ok = match (cred, b64_decode(b)) {
                    (Some(Credential::Basic(u, p)), Some(d)) => d == format!("{u}:{p}"),
                    _ => false,
                };
                ("basic", ok)
            } else {
                ("other", false)
            }
        }
    };
    (scheme.to_string(), ok || cred.is_none())
}

async fn handle(State(state): State<Shared>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let bytes = axum::body::to_bytes(body, usize::MAX)
        .await
        .unwrap_or_default();
    let body_json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    let method = parts.method.as_str().to_string();
    let raw_path = parts.uri.path().to_string();
    let query = parts.uri.query().unwrap_or("").to_string();
    let auth_header = parts
        .headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    let step_and_response = {
        let mut s = state.lock().expect("fake state");
        let (scheme, auth_ok) = check_auth(&s.credential, auth_header.as_deref());
        s.log.push(LoggedRequest {
            method: method.clone(),
            path: raw_path.clone(),
            query: query.clone(),
            auth_scheme: scheme,
            auth_ok,
            body: body_json.clone(),
        });
        let segments: Vec<String> = raw_path
            .trim_start_matches('/')
            .split('/')
            .map(percent_decode)
            .collect();
        let decoded_path = format!("/{}", segments.join("/"));
        if raw_path != "/health" && !auth_ok {
            return json_response(401, json!({"error": "unauthorized"}));
        }
        if let Some((_, _, status, body)) = s
            .failures
            .iter()
            .find(|(m, p, _, _)| *m == method && (*p == decoded_path || *p == raw_path))
        {
            return raw_response(*status, body.clone());
        }
        route(&mut s, &method, &segments, &query, &body_json)
    };
    match step_and_response {
        Routed::Now(status, body) => json_response(status, body),
        Routed::Prompt(step) => run_prompt_step(step).await,
    }
}

enum Routed {
    Now(u16, Value),
    Prompt(PromptStep),
}

fn not_found(what: &str) -> Routed {
    Routed::Now(404, json!({ "error": what }))
}

fn route(s: &mut Inner, method: &str, seg: &[String], query: &str, body: &Value) -> Routed {
    let seg: Vec<&str> = seg.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        ("GET", ["health"]) => Routed::Now(200, json!({"status": "ok"})),
        ("POST", ["sessions"]) => {
            let cwd = body
                .get("cwd")
                .and_then(Value::as_str)
                .unwrap_or("/work")
                .to_string();
            let mut out = json!({});
            if let Some(id) = body.get("resume").and_then(Value::as_str) {
                if !s.sessions.contains_key(id) {
                    match s.dormant.remove(id) {
                        Some(log) => {
                            s.sessions.insert(id.to_string(), log);
                        }
                        None => return not_found("unknown session"),
                    }
                }
                out["sessionId"] = json!(id);
                out["workspace"] = json!(cwd);
                out["resumed"] = json!(true);
            } else {
                s.next_session += 1;
                let id = format!("srv-{}", s.next_session);
                s.sessions.insert(id.clone(), Vec::new());
                out["sessionId"] = json!(id);
                out["workspace"] = json!(cwd);
            }
            if let Some(p) = body.get("profile").and_then(Value::as_str) {
                out["profile"] = json!(p);
            }
            Routed::Now(200, out)
        }
        ("GET", ["sessions", id] | ["sessions", id, "status"]) => match s.sessions.get(*id) {
            Some(log) => Routed::Now(
                200,
                json!({"id": id, "entries": log.len(), "busy": false, "usage_report": null}),
            ),
            None => not_found("unknown session"),
        },
        ("GET", ["sessions", id, "entries"]) => {
            let Some(log) = s.sessions.get(*id) else {
                return not_found("unknown session");
            };
            let after = query_param(query, "after")
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0);
            let limit = query_param(query, "limit")
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(100);
            let rest: Vec<&Value> = log
                .iter()
                .filter(|e| e["seq"].as_u64().unwrap_or(0) > after)
                .collect();
            let page: Vec<Value> = rest.iter().take(limit).map(|e| (*e).clone()).collect();
            let next_after = page.last().and_then(|e| e["seq"].as_u64()).unwrap_or(after);
            let last_seq = log.last().and_then(|e| e["seq"].as_u64()).unwrap_or(0);
            Routed::Now(
                200,
                json!({"id": id, "entries": page, "nextAfter": next_after,
                       "hasMore": rest.len() > limit, "lastSeq": last_seq}),
            )
        }
        ("POST", ["sessions", id, "prompt"]) => {
            if !s.sessions.contains_key(*id) {
                if s.unknown_prompt_500 {
                    // The real server answers 500, not 404, here.
                    return Routed::Now(500, json!({"error": format!("unknown session: {id}")}));
                }
                return not_found("unknown session");
            }
            if body.get("text").and_then(Value::as_str).is_none() {
                return Routed::Now(400, json!({"error": "missing text"}));
            }
            Routed::Prompt(
                s.prompts
                    .pop_front()
                    .unwrap_or_else(|| PromptStep::reply("end_turn", Some("ok"))),
            )
        }
        ("POST", ["sessions", id, "cancel"]) => {
            if s.sessions.contains_key(*id) {
                Routed::Now(200, json!({"cancelled": true, "sessionId": id}))
            } else {
                not_found("unknown session")
            }
        }
        ("GET", ["approvals"]) => {
            let session = query_param(query, "session");
            let list: Vec<&Value> = s
                .approvals
                .iter()
                .filter(|a| session.as_deref().is_none_or(|sid| a["sessionId"] == sid))
                .collect();
            Routed::Now(200, json!({ "approvals": list }))
        }
        ("POST", ["approvals", id, "decision"]) => {
            let decision = match body.get("decision").and_then(Value::as_str) {
                Some(d @ ("allow" | "deny")) => d,
                _ => {
                    return Routed::Now(400, json!({"error": "decision must be 'allow' or 'deny'"}))
                }
            };
            let Some(row) = s.approvals.iter_mut().find(|a| a["id"] == *id) else {
                return not_found("unknown approval id");
            };
            let allow = decision == "allow";
            let status = if allow { "approved" } else { "denied" };
            row["status"] = json!(status);
            s.decisions.push((id.to_string(), decision.to_string()));
            if allow {
                Routed::Now(202, json!({"id": id, "status": status, "resuming": true}))
            } else {
                Routed::Now(200, json!({"id": id, "status": status}))
            }
        }
        _ => not_found("no such route"),
    }
}

async fn run_prompt_step(step: PromptStep) -> Response {
    let mut step = step;
    while let PromptStep::Delayed(d, inner) = step {
        tokio::time::sleep(d).await;
        step = *inner;
    }
    match step {
        PromptStep::Reply { stop_reason, text } => {
            let mut b = json!({ "stopReason": stop_reason });
            if let Some(t) = text {
                b["text"] = json!(t);
            }
            json_response(200, b)
        }
        PromptStep::ApprovalRequired { approval } => {
            let mut b = json!({"stopReason": "approval_required"});
            if let Some(a) = approval {
                b["approval"] = a;
            }
            json_response(200, b)
        }
        PromptStep::Error { status, text } => json_response(status, json!({ "error": text })),
        PromptStep::Delayed(..) => unreachable!("delays are unwrapped above"),
    }
}
