//! End-to-end: the gateway's real pieces against a REAL `tole serve` process.
//!
//! The gateway side is the production code: `HttpToleClient`,
//! `TelegramAdapter` (against the fake Telegram Bot API) and `App` with its
//! run loop. The tole side is the real binary, talking to a fake
//! OpenAI-compatible provider. Every test is `#[ignore]` because it needs the
//! binary:
//!
//! ```text
//! TOLE_BIN=/path/to/tole cargo test -p tole-gateway --test e2e -- --ignored --test-threads=1
//! ```
//!
//! See `docs/e2e.md`.

#[path = "support/fake_provider.rs"]
mod fake_provider;
#[path = "support/fake_telegram.rs"]
mod fake_telegram;

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tempfile::TempDir;
use tole_gateway::app::{Announce, App, AppSettings};
use tole_gateway::auth::{Authorizer, PairingCode, PairingCodeSource, PairingError};
use tole_gateway::channel::UserId;
use tole_gateway::config::ToleAuth;
use tole_gateway::secret::Secret;
use tole_gateway::sessions::{SessionIndex, SystemClock};
use tole_gateway::telegram::{TelegramAdapter, TelegramConfig};
use tole_gateway::tole_client::{HttpToleClient, ToleApi, ToleClientConfig};
use tole_gateway::types::{Decision, PromptOutcome};

use fake_provider::{FakeProvider, WRITE_CONTENT};
use fake_telegram::FakeTelegram;

const OWNER: i64 = 42;
const TOLE_TOKEN: &str = "e2e-tole-token-0123456789";
const BOT_TOKEN: &str = "123456:E2E-BOT-TOKEN";
const PROVIDER_KEY: &str = "e2e-provider-key";

// --------------------------------------------------------------- real tole

/// Directories and the port of one tole installation; they outlive restarts.
struct ToleEnv {
    root: TempDir,
    port: u16,
}

impl ToleEnv {
    fn new() -> Self {
        let root = TempDir::new().unwrap();
        for d in ["workspace", "sessions", "home", "tmp"] {
            std::fs::create_dir_all(root.path().join(d)).unwrap();
        }
        let token_file = root.path().join("token");
        std::fs::write(&token_file, format!("{TOLE_TOKEN}\n")).unwrap();
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        Self { root, port }
    }

    fn workspace(&self) -> PathBuf {
        self.root.path().join("workspace")
    }

    fn sessions(&self) -> PathBuf {
        self.root.path().join("sessions")
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

/// A running `tole serve`. Killed on drop; its stderr is printed when the
/// test is failing.
struct Tole {
    child: Child,
    stderr: PathBuf,
}

impl Tole {
    fn stderr_text(&self) -> String {
        std::fs::read_to_string(&self.stderr).unwrap_or_default()
    }

    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Tole {
    fn drop(&mut self) {
        self.stop();
        if std::thread::panicking() {
            let text = self.stderr_text();
            let tail: String = text
                .chars()
                .rev()
                .take(4000)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            eprintln!("--- tole serve stderr (tail) ---\n{tail}\n--- end ---");
        }
    }
}

fn tole_bin() -> PathBuf {
    let path = std::env::var("TOLE_BIN").expect("TOLE_BIN must point at the tole binary");
    let path = PathBuf::from(path);
    assert!(path.is_file(), "TOLE_BIN is not a file: {}", path.display());
    path
}

/// Start `tole serve` in a fully isolated environment and wait for `/health`.
async fn spawn_tole(env: &ToleEnv, provider_url: &str, log_name: &str) -> Tole {
    let root = env.root.path();
    let stderr_path = root.join(log_name);
    let stderr = std::fs::File::create(&stderr_path).unwrap();
    let mut cmd = Command::new(tole_bin());
    cmd.env_clear();
    if let Some(path) = std::env::var_os("PATH") {
        cmd.env("PATH", path);
    }
    cmd.arg("serve")
        .args(["--bind", "127.0.0.1"])
        .args(["--port", &env.port.to_string()])
        .arg("--token-file")
        .arg(root.join("token"))
        .arg("--workspace")
        .arg(env.workspace())
        .arg("--sessions-dir")
        .arg(env.sessions())
        .arg("--no-auto-mcp")
        .current_dir(env.workspace())
        .env("HOME", root.join("home"))
        .env("CODECORA_HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", root.join("home/.config"))
        .env("XDG_DATA_HOME", root.join("home/.local/share"))
        .env("XDG_CACHE_HOME", root.join("home/.cache"))
        .env("XDG_STATE_HOME", root.join("home/.local/state"))
        .env("TMPDIR", root.join("tmp"))
        .env("TOLE_CRON_DIR", root.join("home/cron"))
        .env("TOLE_NO_UPDATE_CHECK", "1")
        .env("TOLE_BASE_URL", provider_url)
        .env("TOLE_API_KEY", PROVIDER_KEY)
        .env("TOLE_MODEL", "fake-model")
        .env("TOLE_STREAM", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr);
    let child = cmd.spawn().expect("spawn tole");
    let mut tole = Tole {
        child,
        stderr: stderr_path,
    };

    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let health = format!("{}/health", env.url());
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(Some(status)) = tole.child.try_wait() {
            panic!("tole serve exited early ({status}): {}", tole.stderr_text());
        }
        if let Ok(resp) = http.get(&health).send().await {
            if resp.status().is_success() {
                return tole;
            }
        }
        assert!(
            Instant::now() < deadline,
            "tole serve did not become healthy: {}",
            tole.stderr_text()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

// ----------------------------------------------------------- gateway rig

#[derive(Clone)]
struct FixedSource;

impl PairingCodeSource for FixedSource {
    fn fill(&mut self, buf: &mut [u8]) -> Result<(), PairingError> {
        buf.fill(7);
        Ok(())
    }
}

struct Rig {
    provider: FakeProvider,
    tg: FakeTelegram,
    env: ToleEnv,
    tole: Option<Tole>,
    /// A second client for direct probes of the real server.
    client: HttpToleClient,
    runner: tokio::task::JoinHandle<()>,
    next_log: u32,
    _state: TempDir,
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.runner.abort();
    }
}

fn client_for(env: &ToleEnv) -> HttpToleClient {
    HttpToleClient::new(ToleClientConfig {
        base_url: env.url(),
        auth: ToleAuth::Bearer(Secret::new(TOLE_TOKEN)),
        request_timeout: Duration::from_secs(10),
        prompt_timeout: Duration::from_secs(60),
    })
    .unwrap()
}

impl Rig {
    async fn start() -> Rig {
        let provider = FakeProvider::start().await;
        let tg = FakeTelegram::start().await;
        let env = ToleEnv::new();
        let tole = spawn_tole(&env, &provider.url(), "tole-1.stderr").await;
        let state = TempDir::new().unwrap();
        let adapter = TelegramAdapter::new(TelegramConfig {
            api_base: tg.url(),
            bot_token: Secret::new(BOT_TOKEN),
            poll_timeout: Duration::from_secs(1),
        })
        .unwrap();
        let index = SessionIndex::open(state.path().join("sessions.json")).unwrap();
        let auth = Authorizer::new(
            [UserId(OWNER)],
            Duration::from_secs(600),
            SystemClock,
            FixedSource,
        );
        let announced = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = Arc::clone(&announced);
        let announce: Announce = Arc::new(move |code: &PairingCode| {
            sink.lock().unwrap().push(code.reveal().to_owned());
        });
        let settings = AppSettings {
            poll_interval: Duration::from_millis(100),
            resume_grace: Duration::from_secs(10),
            typing_interval: Duration::from_secs(30),
            sweep_interval: Duration::from_secs(3600),
            ..AppSettings::default()
        };
        let app = Arc::new(App::new(
            client_for(&env),
            adapter.clone(),
            SystemClock,
            settings,
            auth,
            index,
            false,
            announce,
        ));
        let runner = {
            let app = Arc::clone(&app);
            tokio::spawn(async move {
                app.run(&adapter, std::future::pending::<()>()).await;
            })
        };
        let client = client_for(&env);
        Rig {
            provider,
            tg,
            env,
            tole: Some(tole),
            client,
            runner,
            next_log: 2,
            _state: state,
        }
    }

    /// Stop the tole process, keeping its sessions dir and port.
    fn stop_tole(&mut self) {
        if let Some(mut t) = self.tole.take() {
            t.stop();
        }
    }

    /// Start a new tole process on the same dirs and port.
    async fn start_tole(&mut self) {
        let name = format!("tole-{}.stderr", self.next_log);
        self.next_log += 1;
        self.tole = Some(spawn_tole(&self.env, &self.provider.url(), &name).await);
    }

    fn say(&self, text: &str) {
        self.tg.push_text(OWNER, OWNER, text);
    }

    /// Everything the bot sent to the chat, in order.
    fn texts(&self) -> Vec<String> {
        self.tg
            .sent_messages()
            .iter()
            .filter_map(|m| m["text"].as_str().map(str::to_owned))
            .collect()
    }

    /// Wait until some bot message satisfies `pred`; returns it.
    async fn wait_text(&self, what: &str, pred: impl Fn(&str) -> bool) -> String {
        let deadline = Instant::now() + Duration::from_secs(40);
        loop {
            if let Some(t) = self.texts().into_iter().find(|t| pred(t)) {
                return t;
            }
            if Instant::now() > deadline {
                let stderr = self
                    .tole
                    .as_ref()
                    .map(Tole::stderr_text)
                    .unwrap_or_default();
                panic!(
                    "timed out waiting for {what}; the chat saw {:?}; tole stderr: {stderr}",
                    self.texts()
                );
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// The callback data of button `index` (0 Allow, 1 Deny) of the newest
    /// approval message.
    fn button(&self, index: usize) -> String {
        let sent = self.tg.sent_messages();
        let msg = sent
            .iter()
            .rev()
            .find(|m| m["reply_markup"]["inline_keyboard"].is_array())
            .expect("an approval message with buttons");
        msg["reply_markup"]["inline_keyboard"][0][index]["callback_data"]
            .as_str()
            .expect("callback data")
            .to_owned()
    }

    fn press(&self, index: usize) {
        let data = self.button(index);
        self.tg.push_callback(OWNER, OWNER, "cb-1", &data, 1000);
    }

    fn workspace_file(&self, rel: &str) -> PathBuf {
        self.env.workspace().join(rel)
    }

    fn assert_no_secrets(&self) {
        for t in self.texts() {
            for secret in [TOLE_TOKEN, BOT_TOKEN, PROVIDER_KEY] {
                assert!(!t.contains(secret), "a secret leaked into the chat: {t}");
            }
        }
    }
}

fn read_index(rig: &Rig) -> Value {
    let text = std::fs::read_to_string(rig._state.path().join("sessions.json")).unwrap();
    serde_json::from_str(&text).unwrap()
}

/// The session ids found as `*.jsonl` files in the sessions dir.
fn session_files(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if let Some(id) = name.strip_suffix(".jsonl") {
                out.push(id.to_owned());
            }
        }
    }
    out.sort();
    out
}

// ------------------------------------------------------------- scenarios

#[tokio::test]
#[ignore = "needs TOLE_BIN"]
async fn a_plain_message_is_answered_in_the_chat() {
    let rig = Rig::start().await;
    rig.say("hello there");
    let answer = rig.wait_text("the echo", |t| t.starts_with("echo:")).await;
    assert_eq!(answer, "echo: hello there");
    let reqs = rig.provider.requests();
    assert!(!reqs.is_empty());
    assert!(reqs.iter().all(|r| r.had_bearer), "provider key reached it");
    rig.assert_no_secrets();
}

#[tokio::test]
#[ignore = "needs TOLE_BIN"]
async fn approval_allow_writes_the_file_and_the_final_answer_reaches_the_chat() {
    let rig = Rig::start().await;
    rig.say("please WRITE notes.md now");
    let ask = rig
        .wait_text("the approval message", |t| t.contains("write_file"))
        .await;
    assert!(ask.contains("notes.md"), "{ask}");
    assert!(
        !rig.workspace_file("notes.md").exists(),
        "nothing is written before the decision"
    );
    assert!(rig.button(0) != rig.button(1));

    rig.press(0);
    rig.wait_text("the final answer", |t| t == "done: notes.md")
        .await;
    assert_eq!(
        std::fs::read_to_string(rig.workspace_file("notes.md")).unwrap(),
        WRITE_CONTENT
    );
    assert!(
        rig.tg
            .edits()
            .iter()
            .any(|e| e["text"].as_str().is_some_and(|t| t.contains("allowed"))),
        "the approval message shows the decision"
    );
    rig.assert_no_secrets();
}

#[tokio::test]
#[ignore = "needs TOLE_BIN"]
async fn approval_deny_leaves_the_file_unwritten_and_says_so() {
    let rig = Rig::start().await;
    rig.say("WRITE denied.md");
    rig.wait_text("the approval message", |t| t.contains("write_file"))
        .await;
    rig.press(1);
    rig.wait_text("the denial notice", |t| t.contains("Denied"))
        .await;
    // Give a (wrongly) resumed turn time to write.
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(!rig.workspace_file("denied.md").exists());
    assert!(
        rig.tg
            .edits()
            .iter()
            .any(|e| e["text"].as_str().is_some_and(|t| t.contains("denied"))),
        "the approval message shows the decision"
    );
    // The session is still usable afterwards.
    rig.say("anything else");
    rig.wait_text("an answer after the denial", |t| {
        t.starts_with("echo:") && t.contains("anything else")
    })
    .await;
    assert!(!rig.workspace_file("denied.md").exists());
    rig.assert_no_secrets();
}

#[tokio::test]
#[ignore = "needs TOLE_BIN"]
async fn the_same_session_is_reused_for_a_second_message() {
    let rig = Rig::start().await;
    rig.say("first");
    rig.wait_text("first answer", |t| t == "echo: first").await;
    rig.say("second");
    rig.wait_text("second answer", |t| t == "echo: second")
        .await;
    assert_eq!(
        session_files(&rig.env.sessions()).len(),
        1,
        "one session file on disk"
    );
    // The provider saw the first turn's messages again in the second request.
    let last = rig.provider.requests().last().cloned().unwrap();
    let all = last.body["messages"].to_string();
    assert!(all.contains("first") && all.contains("second"), "{all}");
}

#[tokio::test]
#[ignore = "needs TOLE_BIN"]
async fn the_gateway_recovers_after_tole_restarts() {
    let mut rig = Rig::start().await;
    rig.say("before");
    rig.wait_text("answer before", |t| t == "echo: before")
        .await;
    let sid = read_index(&rig).to_string();
    let files_before = session_files(&rig.env.sessions());
    assert_eq!(files_before.len(), 1);

    rig.stop_tole();
    rig.start_tole().await;

    rig.say("after");
    rig.wait_text("answer after the restart", |t| t == "echo: after")
        .await;
    // The session was resumed from its file, not replaced.
    assert_eq!(session_files(&rig.env.sessions()), files_before);
    assert!(sid.contains(&files_before[0]), "{sid}");
    assert!(
        !rig.texts()
            .iter()
            .any(|t| t.contains("problem") || t.contains("gone")),
        "the gateway recovered silently: {:?}",
        rig.texts()
    );
    let last = rig.provider.requests().last().cloned().unwrap();
    assert!(
        last.body["messages"].to_string().contains("before"),
        "the resumed session kept its history"
    );
    rig.assert_no_secrets();
}

#[tokio::test]
#[ignore = "needs TOLE_BIN"]
async fn cancel_stops_a_slow_turn() {
    let rig = Rig::start().await;
    rig.say("SLOW please");
    // Wait until tole has asked the provider, so the turn is running.
    let deadline = Instant::now() + Duration::from_secs(20);
    while rig.provider.requests().is_empty() {
        assert!(Instant::now() < deadline, "the turn never started");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    rig.say("/cancel");
    rig.wait_text("the cancel notice", |t| t.contains("Cancel requested"))
        .await;
    rig.wait_text("the turn's end", |t| t == "Cancelled.").await;
    assert!(
        !rig.texts().iter().any(|t| t.starts_with("echo:")),
        "a cancelled turn must not deliver an answer: {:?}",
        rig.texts()
    );
    // The session accepts a new message afterwards.
    eprintln!("chat after cancel: {:?}", rig.texts());
    rig.say("again");
    rig.wait_text("answer after cancel", |t| t == "echo: again")
        .await;
    rig.assert_no_secrets();
}

// ------------------------------------------------- wire contract probes

/// Raw HTTP against the real server, to check the shapes the gateway
/// assumes. Prints nothing secret.
struct Raw {
    http: reqwest::Client,
    base: String,
}

impl Raw {
    fn new(env: &ToleEnv) -> Self {
        Self {
            http: reqwest::Client::builder().no_proxy().build().unwrap(),
            base: env.url(),
        }
    }

    async fn get(&self, path: &str) -> (u16, Value) {
        let r = self
            .http
            .get(format!("{}{path}", self.base))
            .bearer_auth(TOLE_TOKEN)
            .send()
            .await
            .unwrap();
        let status = r.status().as_u16();
        (status, r.json().await.unwrap_or(Value::Null))
    }

    async fn post(&self, path: &str, body: Value) -> (u16, Value) {
        let r = self
            .http
            .post(format!("{}{path}", self.base))
            .bearer_auth(TOLE_TOKEN)
            .json(&body)
            .timeout(Duration::from_secs(60))
            .send()
            .await
            .unwrap();
        let status = r.status().as_u16();
        (status, r.json().await.unwrap_or(Value::Null))
    }
}

#[tokio::test]
#[ignore = "needs TOLE_BIN"]
async fn the_real_wire_shapes_match_what_the_gateway_assumes() {
    let mut rig = Rig::start().await;
    rig.runner.abort(); // no chat traffic here: drive the server directly
    let raw = Raw::new(&rig.env);

    // Session id and what survives percent-encoding.
    let (code, open) = raw.post("/sessions", json!({})).await;
    assert_eq!(code, 200, "{open}");
    let sid = open["sessionId"].as_str().unwrap().to_owned();
    eprintln!("session open: {open}");
    assert_eq!(
        url_encoded(&sid),
        sid,
        "the id has only unreserved characters"
    );

    // A plain turn: the final message entry shape.
    let (code, reply) = raw
        .post(&format!("/sessions/{sid}/prompt"), json!({"text": "hi"}))
        .await;
    assert_eq!(code, 200, "{reply}");
    eprintln!("prompt reply: {reply}");
    assert_eq!(reply["stopReason"], "end_turn");
    assert_eq!(reply["text"], "echo: hi");
    let (_, entries) = raw.get(&format!("/sessions/{sid}/entries")).await;
    eprintln!("entries: {entries}");
    let list = entries["entries"].as_array().unwrap();
    let assistant = list
        .iter()
        .rev()
        .find(|e| e["type"] == "message" && e["payload"]["role"] == "assistant")
        .expect("an assistant message entry");
    assert_eq!(assistant["payload"]["text"], "echo: hi");
    assert!(list
        .windows(2)
        .all(|w| w[0]["seq"].as_u64() < w[1]["seq"].as_u64()));

    // A Write: approval_required with the row.
    let (code, stop) = raw
        .post(
            &format!("/sessions/{sid}/prompt"),
            json!({"text": "WRITE wire.md"}),
        )
        .await;
    assert_eq!(code, 200, "{stop}");
    eprintln!("approval stop: {stop}");
    assert_eq!(stop["stopReason"], "approval_required");
    let row = &stop["approval"];
    for field in ["id", "sessionId", "tool", "description", "status"] {
        assert!(row[field].is_string(), "row.{field} missing: {row}");
    }
    for field in ["createdAtMs", "expiresInSeconds"] {
        assert!(row[field].is_u64(), "row.{field} missing: {row}");
    }
    assert_eq!(row["tool"], "write_file");
    assert_eq!(row["status"], "pending");
    assert_eq!(row["sessionId"], sid.as_str());
    let aid = row["id"].as_str().unwrap().to_owned();
    assert_eq!(url_encoded(&aid), aid, "approval id: {aid}");

    let (_, listed) = raw.get(&format!("/approvals?session={sid}")).await;
    eprintln!("approvals: {listed}");
    assert_eq!(listed["approvals"][0]["id"], aid.as_str());
    let (_, other) = raw.get("/approvals?session=nope").await;
    assert_eq!(
        other["approvals"].as_array().map(Vec::len),
        Some(0),
        "{other}"
    );

    // Allow: 202 resuming, busy while it runs, answer lands in the log.
    let (_, before) = raw.get(&format!("/sessions/{sid}/entries")).await;
    let baseline = before["lastSeq"].as_u64().unwrap();
    let (code, ack) = raw
        .post(
            &format!("/approvals/{aid}/decision"),
            json!({"decision": "allow"}),
        )
        .await;
    eprintln!("decision ack: {code} {ack}");
    assert_eq!(code, 202);
    assert_eq!(ack["resuming"], true);
    assert_eq!(ack["status"], "approved");
    let mut saw_busy = false;
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut final_text = None;
    while Instant::now() < deadline {
        let (_, status) = raw.get(&format!("/sessions/{sid}/status")).await;
        if status["busy"] == true {
            saw_busy = true;
        }
        let (_, page) = raw
            .get(&format!("/sessions/{sid}/entries?after={baseline}"))
            .await;
        final_text = page["entries"]
            .as_array()
            .unwrap()
            .iter()
            .rev()
            .find(|e| e["type"] == "message" && e["payload"]["role"] == "assistant")
            .and_then(|e| e["payload"]["text"].as_str().map(str::to_owned));
        if final_text.is_some() && status["busy"] == false {
            eprintln!("final status: {status}");
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    eprintln!("saw busy during the resume: {saw_busy}");
    let (_, dump) = raw.get(&format!("/sessions/{sid}/entries")).await;
    eprintln!("ALL ENTRIES: {dump}");
    assert_eq!(final_text.as_deref(), Some("done: wire.md"));
    assert_eq!(
        std::fs::read_to_string(rig.workspace_file("wire.md")).unwrap(),
        WRITE_CONTENT
    );
    let (_, ap) = raw.get(&format!("/approvals?session={sid}")).await;
    eprintln!("approvals after allow: {ap}");

    // Restart: what a prompt and a resume do against the new process.
    let (_, full) = raw.get(&format!("/sessions/{sid}/entries")).await;
    let seqs_before = full["lastSeq"].as_u64().unwrap();
    rig.stop_tole();
    rig.start_tole().await;
    let (code, body) = raw
        .post(&format!("/sessions/{sid}/prompt"), json!({"text": "x"}))
        .await;
    eprintln!("prompt after restart: {code} {body}");
    // The real server answers 500, not 404, for a prompt to a session it no
    // longer holds; the gateway must treat that text as "unknown session".
    assert_eq!(code, 500);
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .starts_with("unknown session"),
        "{body}"
    );
    let (code, resumed) = raw.post("/sessions", json!({"resume": sid})).await;
    eprintln!("resume after restart: {code} {resumed}");
    assert_eq!(code, 200);
    assert_eq!(resumed["sessionId"], sid.as_str());
    assert_eq!(resumed["resumed"], true);
    let (_, again) = raw.get(&format!("/sessions/{sid}/entries")).await;
    assert_eq!(again["lastSeq"].as_u64().unwrap(), seqs_before);
    let (code, none) = raw
        .post("/sessions", json!({"resume": "srv-nonexistent"}))
        .await;
    eprintln!("resume unknown: {code} {none}");
    assert_eq!(code, 404);

    // The typed client agrees with the raw reading.
    let st = rig.client.status(&sid).await.unwrap();
    assert!(!st.busy);
    let out = rig.client.prompt(&sid, "typed").await.unwrap();
    assert!(matches!(
        out,
        PromptOutcome::Reply { ref text, .. } if text.as_deref() == Some("echo: typed")
    ));
    let denied = rig.client.prompt(&sid, "WRITE typed.md").await.unwrap();
    let PromptOutcome::ApprovalRequired {
        approval: Some(row),
    } = denied
    else {
        panic!("expected an approval");
    };
    let ack = rig.client.decide(&row.id, Decision::Deny).await.unwrap();
    assert_eq!(ack.status, "denied");
    assert!(!ack.resuming);
}

/// What the client's percent-encoding does to `s`.
fn url_encoded(s: &str) -> String {
    let url = reqwest::Url::parse("http://h/").unwrap();
    let mut url = url;
    url.path_segments_mut().unwrap().pop_if_empty().push(s);
    url.path().trim_start_matches('/').to_owned()
}
