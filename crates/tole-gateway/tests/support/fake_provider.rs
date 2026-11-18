//! A fake OpenAI-compatible LLM server, for the end-to-end tests that run a
//! real `tole serve` (see `tests/e2e.rs`).
//!
//! Include it with `#[path = "support/fake_provider.rs"] mod fake_provider;`.
//! tole is pointed at it with `TOLE_BASE_URL=<url()>`, `TOLE_API_KEY`,
//! `TOLE_MODEL` and `TOLE_STREAM=0` (non-streaming). tole posts to
//! `<base>/chat/completions` with `Authorization: Bearer <key>` and a body
//! `{model, messages, tools, ...}`.
//!
//! The behavior is stateless and driven by the request:
//!
//! - the last message has role `tool`: a final text `done: <path>` (the path
//!   comes from the most recent `WRITE <path>` in the conversation);
//! - the last user message contains `WRITE <relative path>`: a `tool_calls`
//!   answer calling `write_file` with `{"path": <path>, "content": ...}`;
//! - the last user message is tole's denial note (`[tole] Your call to ...`),
//!   which the real server appends when it resumes a turn that was parked on
//!   an approval: the call is REPEATED unchanged (the owner's allow is a
//!   one-shot for exactly that call), as a model following the note would;
//! - otherwise a final text `echo: <last user text>`.
//!
//! A last user message containing `SLOW` delays the answer by 2 seconds.
//! Every request is recorded (`requests()`); the API key is never stored.

#![allow(dead_code)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};
use tokio::task::JoinHandle;

/// The content the fake asks `write_file` to write.
pub const WRITE_CONTENT: &str = "written by the fake provider\n";

/// How tole's denial note to the model begins.
const DENIAL_NOTE_PREFIX: &str = "[tole] Your call to";

/// One request the fake received.
#[derive(Debug, Clone)]
pub struct ProviderRequest {
    /// The JSON body.
    pub body: Value,
    /// True when `Authorization: Bearer <key>` carried a non-empty key.
    pub had_bearer: bool,
}

#[derive(Default)]
struct Shared {
    requests: Mutex<Vec<ProviderRequest>>,
}

/// The fake provider handle; it serves until dropped.
pub struct FakeProvider {
    url: String,
    shared: Arc<Shared>,
    task: JoinHandle<()>,
}

impl Drop for FakeProvider {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl FakeProvider {
    /// Start on an ephemeral loopback port.
    pub async fn start() -> FakeProvider {
        let shared = Arc::new(Shared::default());
        let app = Router::new()
            .route("/chat/completions", post(completions))
            .route("/v1/chat/completions", post(completions))
            .with_state(shared.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        FakeProvider { url, shared, task }
    }

    /// `http://127.0.0.1:<port>`: the value for `TOLE_BASE_URL`.
    pub fn url(&self) -> String {
        self.url.clone()
    }

    /// Every request so far, oldest first.
    pub fn requests(&self) -> Vec<ProviderRequest> {
        self.shared.requests.lock().unwrap().clone()
    }
}

/// The text of a message `content`: a string, or the concatenated `text`
/// parts of a content array.
fn content_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// The relative path after `WRITE ` in `text`, when present.
fn write_path(text: &str) -> Option<String> {
    let rest = text.split("WRITE ").nth(1)?;
    let path = rest.split_whitespace().next()?;
    Some(path.to_owned())
}

async fn completions(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    let had_bearer = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.strip_prefix("Bearer ").is_some_and(|k| !k.is_empty()));
    shared.requests.lock().unwrap().push(ProviderRequest {
        body: body.clone(),
        had_bearer,
    });

    let messages = body["messages"].as_array().cloned().unwrap_or_default();
    let last_is_tool = messages.last().is_some_and(|m| m["role"] == "tool");
    let last_user = messages
        .iter()
        .rev()
        .find(|m| m["role"] == "user")
        .map(|m| content_text(&m["content"]))
        .unwrap_or_default();
    if last_user.contains("SLOW") {
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    let message = if last_is_tool {
        let path = messages
            .iter()
            .rev()
            .filter(|m| m["role"] == "user")
            .find_map(|m| write_path(&content_text(&m["content"])))
            .unwrap_or_default();
        json!({"role": "assistant", "content": format!("done: {path}")})
    } else if let Some(path) = write_path(&last_user).or_else(|| {
        // After tole's denial note, repeat the last WRITE the user asked for.
        last_user.starts_with(DENIAL_NOTE_PREFIX).then(|| {
            messages
                .iter()
                .rev()
                .filter(|m| m["role"] == "user")
                .find_map(|m| write_path(&content_text(&m["content"])))
                .unwrap_or_default()
        })
    }) {
        let args = json!({"path": path, "content": WRITE_CONTENT}).to_string();
        json!({"role": "assistant", "content": null, "tool_calls": [{
            "id": "call_write_1", "type": "function",
            "function": {"name": "write_file", "arguments": args}}]})
    } else {
        json!({"role": "assistant", "content": format!("echo: {last_user}")})
    };
    let finish = if message.get("tool_calls").is_some() {
        "tool_calls"
    } else {
        "stop"
    };
    Json(json!({
        "id": "chatcmpl-fake",
        "object": "chat.completion",
        "model": body["model"],
        "choices": [{"index": 0, "message": message, "finish_reason": finish}],
        "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
    }))
}
