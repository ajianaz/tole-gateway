//! A fake Telegram Bot API on `127.0.0.1:0`, for tests.
//!
//! Include it with `#[path = "support/fake_telegram.rs"] mod fake_telegram;`.
//!
//! ```ignore
//! let fake = FakeTelegram::start().await;
//! // point the adapter at fake.url(); the token is whatever the client uses.
//! fake.push_text(42, 42, "hello");            // private text message
//! fake.push_callback(42, 42, "cb1", "a:1", 7); // button press on message 7
//! fake.push_update(json!({...}));             // any raw update (id assigned)
//! ```
//!
//! Updates:
//! - `push_update(v)` queues a raw update (an `update_id` is added) and returns
//!   its id. Helpers: `push_text`, `push_callback`, `push_group_text`,
//!   `push_edited`, `push_sticker`, `push_channel_post`.
//! - `getUpdates` honors `offset` (updates below it are acknowledged and
//!   dropped; the rest are redelivered until acknowledged) and simulates the
//!   long poll: with nothing queued it waits up to `timeout` seconds, waking
//!   early when an update is pushed, then answers `{"ok":true,"result":[]}`.
//!
//! Recorders (request bodies as JSON): `sent_messages()`, `edits()`,
//! `callback_answers()`, `chat_actions()`, `commands()`, `poll_requests()`,
//! plus `tokens_seen()` (the token segment of each request path).
//!
//! Failure switches (apply to the next request of any method, once):
//! `rate_limit_once(secs)` (HTTP 429 with `retry_after`),
//! `rate_limit_after(pass, times, secs)` (let `pass` requests through first),
//! `forbid_once()`
//! (HTTP 403 "bot was blocked"), `reject_once(description)` (HTTP 400
//! `ok:false`), `raw_once(status, body)` (any bytes, e.g. bad JSON).
//!
//! `sendMessage` answers a Message with an increasing `message_id` starting
//! at 1000. The server stops when the `FakeTelegram` is dropped.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Router;
use serde_json::{json, Value};
use tokio::sync::Notify;

#[derive(Default)]
struct Inner {
    queue: VecDeque<(i64, Value)>,
    next_update_id: i64,
    next_message_id: i64,
    sent: Vec<Value>,
    edits: Vec<Value>,
    answers: Vec<Value>,
    actions: Vec<Value>,
    commands: Vec<Value>,
    polls: Vec<Value>,
    tokens: Vec<String>,
    /// One entry per upcoming request: `Some` answers with that failure,
    /// `None` lets the request through.
    fail: VecDeque<Option<(u16, Vec<u8>)>>,
}

struct Shared {
    inner: Mutex<Inner>,
    notify: Notify,
}

/// The fake server handle.
pub struct FakeTelegram {
    url: String,
    shared: Arc<Shared>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for FakeTelegram {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl FakeTelegram {
    /// Start on an ephemeral loopback port.
    pub async fn start() -> FakeTelegram {
        let shared = Arc::new(Shared {
            inner: Mutex::new(Inner {
                next_update_id: 100,
                next_message_id: 1000,
                ..Inner::default()
            }),
            notify: Notify::new(),
        });
        let app = Router::new()
            .route("/{bot}/{method}", post(handle))
            .with_state(shared.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        FakeTelegram { url, shared, task }
    }

    /// Base URL to use as `api_base`.
    pub fn url(&self) -> String {
        self.url.clone()
    }

    fn with<R>(&self, f: impl FnOnce(&mut Inner) -> R) -> R {
        f(&mut self.shared.inner.lock().unwrap())
    }

    /// Queue a raw update; the `update_id` is assigned here. Returns it.
    pub fn push_update(&self, mut update: Value) -> i64 {
        let id = self.with(|i| {
            let id = i.next_update_id;
            i.next_update_id += 1;
            update["update_id"] = json!(id);
            i.queue.push_back((id, update));
            id
        });
        self.shared.notify.notify_waiters();
        id
    }

    /// A text message in a private chat.
    pub fn push_text(&self, chat: i64, user: i64, text: &str) -> i64 {
        self.push_update(json!({"message": {
            "message_id": 1, "date": 0,
            "from": {"id": user, "is_bot": false, "first_name": "t"},
            "chat": {"id": chat, "type": "private"},
            "text": text}}))
    }

    /// A text message in a supergroup.
    pub fn push_group_text(&self, chat: i64, user: i64, text: &str) -> i64 {
        self.push_update(json!({"message": {
            "message_id": 1, "date": 0,
            "from": {"id": user, "is_bot": false, "first_name": "t"},
            "chat": {"id": chat, "type": "supergroup"},
            "text": text}}))
    }

    /// An edited private message.
    pub fn push_edited(&self, chat: i64, user: i64, text: &str) -> i64 {
        self.push_update(json!({"edited_message": {
            "message_id": 1, "date": 0,
            "from": {"id": user, "is_bot": false, "first_name": "t"},
            "chat": {"id": chat, "type": "private"},
            "text": text}}))
    }

    /// A sticker (no text) in a private chat.
    pub fn push_sticker(&self, chat: i64, user: i64) -> i64 {
        self.push_update(json!({"message": {
            "message_id": 1, "date": 0,
            "from": {"id": user, "is_bot": false, "first_name": "t"},
            "chat": {"id": chat, "type": "private"},
            "sticker": {"file_id": "x"}}}))
    }

    /// A channel post.
    pub fn push_channel_post(&self, chat: i64, text: &str) -> i64 {
        self.push_update(json!({"channel_post": {
            "message_id": 1, "date": 0,
            "chat": {"id": chat, "type": "channel"},
            "text": text}}))
    }

    /// A button press on `message_id` in a private chat.
    pub fn push_callback(
        &self,
        chat: i64,
        user: i64,
        id: &str,
        data: &str,
        message_id: i64,
    ) -> i64 {
        self.push_update(json!({"callback_query": {
            "id": id, "chat_instance": "ci",
            "from": {"id": user, "is_bot": false, "first_name": "t"},
            "message": {"message_id": message_id, "date": 0,
                        "chat": {"id": chat, "type": "private"}},
            "data": data}}))
    }

    /// Answer the next request with HTTP 429 and `retry_after`.
    pub fn rate_limit_once(&self, retry_after: u64) {
        let body = json!({"ok": false, "error_code": 429,
            "description": format!("Too Many Requests: retry after {retry_after}"),
            "parameters": {"retry_after": retry_after}});
        self.with(|i| i.fail = VecDeque::from([Some((429, body.to_string().into_bytes()))]));
    }

    /// Let `pass` requests through, then answer the next `times` requests
    /// with HTTP 429 and `retry_after`.
    pub fn rate_limit_after(&self, pass: usize, times: usize, retry_after: u64) {
        let body = json!({"ok": false, "error_code": 429,
            "description": format!("Too Many Requests: retry after {retry_after}"),
            "parameters": {"retry_after": retry_after}})
        .to_string()
        .into_bytes();
        self.with(|i| {
            i.fail.clear();
            i.fail.extend((0..pass).map(|_| None));
            i.fail.extend((0..times).map(|_| Some((429, body.clone()))));
        });
    }

    /// Answer the next request with HTTP 403 "bot was blocked".
    pub fn forbid_once(&self) {
        let body = json!({"ok": false, "error_code": 403,
            "description": "Forbidden: bot was blocked by the user"});
        self.with(|i| i.fail = VecDeque::from([Some((403, body.to_string().into_bytes()))]));
    }

    /// Answer the next request with HTTP 400 and `ok:false`.
    pub fn reject_once(&self, description: &str) {
        let body = json!({"ok": false, "error_code": 400, "description": description});
        self.with(|i| i.fail = VecDeque::from([Some((400, body.to_string().into_bytes()))]));
    }

    /// Answer the next request with arbitrary bytes.
    pub fn raw_once(&self, status: u16, body: &str) {
        self.with(|i| i.fail = VecDeque::from([Some((status, body.as_bytes().to_vec()))]));
    }

    /// Bodies of `sendMessage` calls.
    pub fn sent_messages(&self) -> Vec<Value> {
        self.with(|i| i.sent.clone())
    }

    /// Bodies of `editMessageText` calls.
    pub fn edits(&self) -> Vec<Value> {
        self.with(|i| i.edits.clone())
    }

    /// Bodies of `answerCallbackQuery` calls.
    pub fn callback_answers(&self) -> Vec<Value> {
        self.with(|i| i.answers.clone())
    }

    /// Bodies of `sendChatAction` calls.
    pub fn chat_actions(&self) -> Vec<Value> {
        self.with(|i| i.actions.clone())
    }

    /// Bodies of `setMyCommands` calls.
    pub fn commands(&self) -> Vec<Value> {
        self.with(|i| i.commands.clone())
    }

    /// Bodies of `getUpdates` calls.
    pub fn poll_requests(&self) -> Vec<Value> {
        self.with(|i| i.polls.clone())
    }

    /// The token segment (`bot<token>`) of every request path.
    pub fn tokens_seen(&self) -> Vec<String> {
        self.with(|i| i.tokens.clone())
    }

    /// Updates not yet acknowledged through `offset`.
    pub fn pending_updates(&self) -> usize {
        self.with(|i| i.queue.len())
    }
}

fn reply(status: u16, body: Vec<u8>) -> Response {
    (
        StatusCode::from_u16(status).unwrap(),
        [(header::CONTENT_TYPE, "application/json")],
        body,
    )
        .into_response()
}

fn ok(result: Value) -> Response {
    reply(
        200,
        json!({"ok": true, "result": result})
            .to_string()
            .into_bytes(),
    )
}

async fn handle(
    State(shared): State<Arc<Shared>>,
    Path((bot, method)): Path<(String, String)>,
    body: axum::body::Bytes,
) -> Response {
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let injected = {
        let mut i = shared.inner.lock().unwrap();
        i.tokens.push(bot);
        i.fail.pop_front().flatten()
    };
    if let Some((status, bytes)) = injected {
        return reply(status, bytes);
    }
    match method.as_str() {
        "getUpdates" => get_updates(&shared, body).await,
        "sendMessage" => {
            let mut i = shared.inner.lock().unwrap();
            let id = i.next_message_id;
            i.next_message_id += 1;
            let chat = body["chat_id"].clone();
            i.sent.push(body);
            ok(json!({"message_id": id, "date": 0,
                      "chat": {"id": chat, "type": "private"}}))
        }
        "editMessageText" => {
            shared.inner.lock().unwrap().edits.push(body);
            ok(json!(true))
        }
        "answerCallbackQuery" => {
            shared.inner.lock().unwrap().answers.push(body);
            ok(json!(true))
        }
        "sendChatAction" => {
            shared.inner.lock().unwrap().actions.push(body);
            ok(json!(true))
        }
        "setMyCommands" => {
            shared.inner.lock().unwrap().commands.push(body);
            ok(json!(true))
        }
        _ => reply(
            404,
            json!({"ok": false, "error_code": 404, "description": "Not Found"})
                .to_string()
                .into_bytes(),
        ),
    }
}

async fn get_updates(shared: &Arc<Shared>, body: Value) -> Response {
    let timeout = body["timeout"].as_u64().unwrap_or(0);
    let offset = body["offset"].as_i64();
    {
        let mut i = shared.inner.lock().unwrap();
        i.polls.push(body);
        if let Some(o) = offset {
            i.queue.retain(|(id, _)| *id >= o);
        }
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout);
    loop {
        let notified = shared.notify.notified();
        tokio::pin!(notified);
        // Register interest before checking, so a push in between is not lost.
        notified.as_mut().enable();
        {
            let i = shared.inner.lock().unwrap();
            if !i.queue.is_empty() {
                let items: Vec<Value> = i.queue.iter().map(|(_, u)| u.clone()).collect();
                return ok(Value::Array(items));
            }
        }
        if tokio::time::timeout_at(deadline, notified).await.is_err() {
            return ok(json!([]));
        }
    }
}
