//! The gateway app against a fake `tole serve` and a fake Telegram Bot API,
//! through the real HTTP client and the real Telegram adapter.

#[path = "support/fake_telegram.rs"]
mod fake_telegram;
#[path = "support/fake_tole.rs"]
mod fake_tole;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tempfile::TempDir;
use tole_gateway::app::{Announce, App, AppSettings};
use tole_gateway::auth::{Authorizer, PairingCode, PairingCodeSource, PairingError};
use tole_gateway::channel::{ChatId, Inbound, UserId};
use tole_gateway::config::ToleAuth;
use tole_gateway::secret::Secret;
use tole_gateway::sessions::{Clock, SessionIndex};
use tole_gateway::telegram::{TelegramAdapter, TelegramConfig};
use tole_gateway::tole_client::{HttpToleClient, ToleClientConfig};

use fake_telegram::FakeTelegram;
use fake_tole::{FakeTole, PromptStep};

const OWNER: i64 = 42;
const TOLE_TOKEN: &str = "tole-secret-token-xyz";
const BOT_TOKEN: &str = "123456:BOT-SECRET-TOKEN";

#[derive(Clone)]
struct TestClock(Arc<AtomicU64>);

impl TestClock {
    fn advance(&self, d: Duration) {
        self.0.fetch_add(d.as_millis() as u64, Ordering::SeqCst);
    }
}

impl Clock for TestClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

struct FixedSource;

impl PairingCodeSource for FixedSource {
    fn fill(&mut self, buf: &mut [u8]) -> Result<(), PairingError> {
        for (i, b) in buf.iter_mut().enumerate() {
            *b = (i * 7 + 3) as u8;
        }
        Ok(())
    }
}

type TestApp = App<HttpToleClient, TelegramAdapter, TestClock, FixedSource>;

struct Rig {
    tole: FakeTole,
    tg: FakeTelegram,
    app: Arc<TestApp>,
    adapter: TelegramAdapter,
    clock: TestClock,
    announced: Arc<Mutex<Vec<String>>>,
    _dir: TempDir,
}

async fn rig(allowed: &[i64]) -> Rig {
    let tole = FakeTole::start().await;
    tole.require_bearer(TOLE_TOKEN);
    let tg = FakeTelegram::start().await;
    let dir = TempDir::new().unwrap();
    // Start at the real time: the fake tole stamps approvals with it, and in
    // production tole and the gateway share the host clock.
    let start_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let clock = TestClock(Arc::new(AtomicU64::new(start_ms)));

    let client = HttpToleClient::new(ToleClientConfig {
        base_url: tole.url(),
        auth: ToleAuth::Bearer(Secret::new(TOLE_TOKEN)),
        request_timeout: Duration::from_secs(5),
        prompt_timeout: Duration::from_secs(10),
    })
    .unwrap();
    let adapter = TelegramAdapter::new(TelegramConfig {
        api_base: tg.url(),
        bot_token: Secret::new(BOT_TOKEN),
        poll_timeout: Duration::from_secs(1),
    })
    .unwrap();
    let index = SessionIndex::open(dir.path().join("sessions.json")).unwrap();
    let auth = Authorizer::new(
        allowed.iter().copied().map(UserId),
        Duration::from_secs(600),
        clock.clone(),
        FixedSource,
    );
    let announced = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&announced);
    let announce: Announce = Arc::new(move |code: &PairingCode| {
        sink.lock().unwrap().push(code.reveal().to_owned());
    });
    let settings = AppSettings {
        poll_interval: Duration::from_millis(20),
        resume_grace: Duration::from_secs(2),
        typing_interval: Duration::from_secs(30),
        sweep_interval: Duration::from_secs(3600),
        ..AppSettings::default()
    };
    let app = Arc::new(App::new(
        client,
        adapter.clone(),
        clock.clone(),
        settings,
        auth,
        index,
        allowed.is_empty(),
        announce,
    ));
    Rig {
        tole,
        tg,
        app,
        adapter,
        clock,
        announced,
        _dir: dir,
    }
}

fn msg(chat: i64, user: i64, text: &str) -> Inbound {
    Inbound::Message {
        chat: ChatId(chat),
        user: UserId(user),
        text: text.to_owned(),
        message_id: 1,
    }
}

fn press(chat: i64, user: i64, data: &str) -> Inbound {
    Inbound::Callback {
        chat: ChatId(chat),
        user: UserId(user),
        callback_id: "cb".to_owned(),
        data: data.to_owned(),
        message: None,
    }
}

fn texts(tg: &FakeTelegram) -> Vec<String> {
    tg.sent_messages()
        .iter()
        .filter_map(|m| m["text"].as_str().map(str::to_owned))
        .collect()
}

fn button(tg: &FakeTelegram, index: usize) -> String {
    let sent = tg.sent_messages();
    let with_buttons = sent
        .iter()
        .rev()
        .find(|m| m["reply_markup"]["inline_keyboard"].is_array())
        .expect("an approval message with buttons");
    with_buttons["reply_markup"]["inline_keyboard"][0][index]["callback_data"]
        .as_str()
        .expect("callback data")
        .to_owned()
}

async fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
    for _ in 0..300 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {what}");
}

fn post_sessions(tole: &FakeTole) -> usize {
    tole.requests()
        .iter()
        .filter(|r| r.method == "POST" && r.path == "/sessions")
        .count()
}

fn assert_no_secrets(rig: &Rig) {
    for t in texts(&rig.tg) {
        assert!(!t.contains(TOLE_TOKEN), "tole token leaked into chat: {t}");
        assert!(!t.contains(BOT_TOKEN), "bot token leaked into chat: {t}");
    }
}

/// An approval-required prompt for session `srv-1`, with the row pre-added.
fn script_approval(tole: &FakeTole, id: &str) {
    let row = tole.add_approval(id, "srv-1", "write_file", "write notes.md");
    tole.script_prompt(PromptStep::approval_required(Some(row)));
}

#[tokio::test]
async fn an_unauthorized_user_gets_no_reply_and_never_reaches_tole() {
    let r = rig(&[OWNER]).await;
    r.app.handle(msg(999, 999, "hello")).await;
    r.app.handle(msg(999, 999, "/status")).await;
    assert!(r.tg.sent_messages().is_empty());
    assert!(r.tole.requests().is_empty());
}

#[tokio::test]
async fn a_message_becomes_a_prompt_and_the_answer_comes_back() {
    let r = rig(&[OWNER]).await;
    r.tole
        .script_prompt(PromptStep::reply("end_turn", Some("hello from tole")));
    r.app.handle(msg(OWNER, OWNER, "hi")).await;
    assert_eq!(texts(&r.tg), vec!["hello from tole".to_owned()]);
    let prompts = r.tole.requests_to("/sessions/srv-1/prompt");
    assert_eq!(prompts.len(), 1);
    assert_eq!(prompts[0].body["text"], "hi");
    assert_no_secrets(&r);
}

#[tokio::test]
async fn the_second_message_reuses_the_session() {
    let r = rig(&[OWNER]).await;
    r.app.handle(msg(OWNER, OWNER, "one")).await;
    r.app.handle(msg(OWNER, OWNER, "two")).await;
    assert_eq!(post_sessions(&r.tole), 1);
    assert_eq!(r.tole.requests_to("/sessions/srv-1/prompt").len(), 2);
}

#[tokio::test]
async fn the_session_rolls_over_after_four_idle_hours_only() {
    let r = rig(&[OWNER]).await;
    r.app.handle(msg(OWNER, OWNER, "one")).await;
    r.clock.advance(Duration::from_secs(3 * 3600));
    r.app.handle(msg(OWNER, OWNER, "two")).await;
    assert_eq!(post_sessions(&r.tole), 1, "3 hours idle must not roll over");
    r.clock.advance(Duration::from_secs(4 * 3600 + 1));
    r.app.handle(msg(OWNER, OWNER, "three")).await;
    assert_eq!(
        post_sessions(&r.tole),
        2,
        "over 4 hours idle must roll over"
    );
    assert!(texts(&r.tg).iter().any(|t| t.contains("new session")));
}

#[tokio::test]
async fn the_idle_clock_counts_owner_messages_only() {
    let r = rig(&[OWNER]).await;
    r.app.handle(msg(OWNER, OWNER, "one")).await;
    // An unauthorized user chattering does not reset the owner's idle time.
    r.clock.advance(Duration::from_secs(3 * 3600));
    r.app.handle(msg(999, 999, "noise")).await;
    r.clock.advance(Duration::from_secs(3600 + 1));
    r.app.handle(msg(OWNER, OWNER, "two")).await;
    assert_eq!(post_sessions(&r.tole), 2);
}

#[tokio::test]
async fn approval_allow_resumes_and_delivers_the_final_answer() {
    let r = rig(&[OWNER]).await;
    script_approval(&r.tole, "apr-1");
    r.app.handle(msg(OWNER, OWNER, "write the notes")).await;

    let all = texts(&r.tg);
    assert!(all.last().unwrap().contains("write_file"), "{all:?}");
    assert!(all.last().unwrap().contains("write notes.md"));
    let allow = button(&r.tg, 0);

    let app = Arc::clone(&r.app);
    let task = tokio::spawn(async move { app.handle(press(OWNER, OWNER, &allow)).await });
    wait_until("the decision", || !r.tole.decisions().is_empty()).await;
    r.tole.add_message("srv-1", "assistant", "notes written");
    task.await.unwrap();

    assert_eq!(
        r.tole.decisions(),
        vec![("apr-1".to_owned(), "allow".to_owned())]
    );
    assert_eq!(texts(&r.tg).last().unwrap(), "notes written");
    assert!(
        r.tg.edits()
            .iter()
            .any(|e| e["text"].as_str().is_some_and(|t| t.contains("allowed"))),
        "the approval message is edited to show the decision"
    );
    assert_no_secrets(&r);
}

#[tokio::test]
async fn approval_deny_records_a_denial_and_runs_nothing() {
    let r = rig(&[OWNER]).await;
    script_approval(&r.tole, "apr-1");
    r.app.handle(msg(OWNER, OWNER, "write the notes")).await;
    let deny = button(&r.tg, 1);
    r.app.handle(press(OWNER, OWNER, &deny)).await;
    assert_eq!(
        r.tole.decisions(),
        vec![("apr-1".to_owned(), "deny".to_owned())]
    );
    assert!(texts(&r.tg).last().unwrap().contains("Denied"));
}

#[tokio::test]
async fn a_button_cannot_be_used_twice_or_by_a_stranger() {
    let r = rig(&[OWNER]).await;
    script_approval(&r.tole, "apr-1");
    r.app.handle(msg(OWNER, OWNER, "write")).await;
    let deny = button(&r.tg, 1);
    // A stranger pressing the owner's button changes nothing.
    r.app.handle(press(OWNER, 999, &deny)).await;
    assert!(r.tole.decisions().is_empty());
    r.app.handle(press(OWNER, OWNER, &deny)).await;
    r.app.handle(press(OWNER, OWNER, &deny)).await;
    assert_eq!(r.tole.decisions().len(), 1, "one press, one decision");
}

#[tokio::test]
async fn a_new_message_while_an_approval_is_pending_is_not_run() {
    let r = rig(&[OWNER]).await;
    script_approval(&r.tole, "apr-1");
    r.app.handle(msg(OWNER, OWNER, "write")).await;
    r.app.handle(msg(OWNER, OWNER, "something else")).await;
    assert_eq!(r.tole.requests_to("/sessions/srv-1/prompt").len(), 1);
    assert!(texts(&r.tg)
        .last()
        .unwrap()
        .contains("Waiting for your decision"));
}

#[tokio::test]
async fn cancel_denies_a_pending_approval() {
    let r = rig(&[OWNER]).await;
    script_approval(&r.tole, "apr-1");
    r.app.handle(msg(OWNER, OWNER, "write")).await;
    r.app.handle(msg(OWNER, OWNER, "/cancel")).await;
    assert_eq!(
        r.tole.decisions(),
        vec![("apr-1".to_owned(), "deny".to_owned())]
    );
    assert!(texts(&r.tg).last().unwrap().contains("denied"));
}

#[tokio::test]
async fn an_unanswered_approval_is_denied_by_the_sweep() {
    let r = rig(&[OWNER]).await;
    script_approval(&r.tole, "apr-1");
    r.app.handle(msg(OWNER, OWNER, "write")).await;
    r.clock.advance(Duration::from_secs(899));
    r.app.sweep().await;
    assert!(r.tole.decisions().is_empty(), "still inside the window");
    r.clock.advance(Duration::from_secs(2));
    r.app.sweep().await;
    assert_eq!(
        r.tole.decisions(),
        vec![("apr-1".to_owned(), "deny".to_owned())]
    );
    assert!(texts(&r.tg)
        .last()
        .unwrap()
        .contains("not answered in time"));
}

#[tokio::test]
async fn new_starts_a_fresh_session_but_not_while_work_is_pending() {
    let r = rig(&[OWNER]).await;
    r.app.handle(msg(OWNER, OWNER, "one")).await;
    r.app.handle(msg(OWNER, OWNER, "/new")).await;
    assert_eq!(post_sessions(&r.tole), 2);
    assert!(texts(&r.tg).last().unwrap().contains("new session"));

    script_approval(&r.tole, "apr-1");
    r.tole.add_session("srv-2");
    r.app.handle(msg(OWNER, OWNER, "write")).await;
    r.app.handle(msg(OWNER, OWNER, "/new")).await;
    assert_eq!(post_sessions(&r.tole), 2, "/new is refused while pending");
    assert!(texts(&r.tg).last().unwrap().contains("Finish or /cancel"));
}

#[tokio::test]
async fn a_second_message_during_a_running_turn_is_refused_not_queued() {
    let r = rig(&[OWNER]).await;
    r.tole.script_prompt(PromptStep::delayed(
        Duration::from_millis(400),
        PromptStep::reply("end_turn", Some("slow answer")),
    ));
    let app = Arc::clone(&r.app);
    let first = tokio::spawn(async move { app.handle(msg(OWNER, OWNER, "slow")).await });
    wait_until("the prompt to start", || {
        !r.tole.requests_to("/sessions/srv-1/prompt").is_empty()
    })
    .await;
    r.app.handle(msg(OWNER, OWNER, "impatient")).await;
    assert!(texts(&r.tg)
        .iter()
        .any(|t| t.contains("Still working on the previous message")));
    first.await.unwrap();
    assert_eq!(r.tole.requests_to("/sessions/srv-1/prompt").len(), 1);
    assert_eq!(texts(&r.tg).last().unwrap(), "slow answer");
}

#[tokio::test]
async fn status_and_help_answer_without_a_turn() {
    let r = rig(&[OWNER]).await;
    r.app.handle(msg(OWNER, OWNER, "/status")).await;
    assert!(texts(&r.tg).last().unwrap().contains("No session yet"));
    r.app.handle(msg(OWNER, OWNER, "hi")).await;
    r.app.handle(msg(OWNER, OWNER, "/status")).await;
    assert!(texts(&r.tg).last().unwrap().contains("srv-1"));
    r.app.handle(msg(OWNER, OWNER, "/help")).await;
    assert!(texts(&r.tg).last().unwrap().contains("/cancel"));
}

#[tokio::test]
async fn tole_failures_reach_the_chat_as_short_sentences_without_secrets() {
    let r = rig(&[OWNER]).await;
    r.tole
        .fail("POST", "/sessions", 500, "internal detail 12345");
    r.app.handle(msg(OWNER, OWNER, "hi")).await;
    let last = texts(&r.tg).last().unwrap().clone();
    assert!(last.contains("tole reported a problem"), "{last}");
    assert_no_secrets(&r);

    r.tole.clear_failures();
    r.tole.require_bearer("a-different-token");
    r.app.handle(msg(OWNER, OWNER, "again")).await;
    let last = texts(&r.tg).last().unwrap().clone();
    assert!(last.contains("not authorized"), "{last}");
    assert_no_secrets(&r);
}

#[tokio::test]
async fn pairing_authorizes_a_stranger_with_the_console_code_only() {
    let r = rig(&[]).await;
    r.app.ensure_pairing();
    let code = r.announced.lock().unwrap()[0].clone();

    r.app.handle(msg(7, 7, "WRONGCODE")).await;
    r.app.handle(msg(7, 7, "hello")).await;
    assert!(r.tg.sent_messages().is_empty(), "wrong code gets no reply");
    assert!(r.tole.requests().is_empty());

    r.app.handle(msg(7, 7, &code)).await;
    assert!(texts(&r.tg).last().unwrap().contains("Paired"));
    r.app.handle(msg(7, 7, "now talk")).await;
    assert_eq!(r.tole.requests_to("/sessions/srv-1/prompt").len(), 1);

    // The code is single use and pairing is closed after the first success.
    r.app.handle(msg(8, 8, &code)).await;
    assert_eq!(r.tole.requests_to("/sessions/srv-1/prompt").len(), 1);
    r.app.handle(msg(8, 8, "hi")).await;
    assert!(r
        .tole
        .requests()
        .iter()
        .all(|q| q.path != "/sessions/srv-2/prompt"));
}

#[tokio::test]
async fn the_pairing_code_is_announced_once_and_refreshed_after_it_expires() {
    let r = rig(&[]).await;
    r.app.ensure_pairing();
    r.app.ensure_pairing();
    assert_eq!(r.announced.lock().unwrap().len(), 1);
    r.clock.advance(Duration::from_secs(601));
    r.app.ensure_pairing();
    assert_eq!(r.announced.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn pressing_an_expired_button_denies_it_and_unblocks_the_chat() {
    let r = rig(&[OWNER]).await;
    script_approval(&r.tole, "apr-1");
    r.app.handle(msg(OWNER, OWNER, "write")).await;
    let allow = button(&r.tg, 0);
    r.clock.advance(Duration::from_secs(901));
    r.app.handle(press(OWNER, OWNER, &allow)).await;
    assert_eq!(
        r.tole.decisions(),
        vec![("apr-1".to_owned(), "deny".to_owned())],
        "an expired press is a denial, never an allow"
    );
    // The chat is not stuck on "Waiting for your decision".
    r.tole
        .script_prompt(PromptStep::reply("end_turn", Some("fresh start")));
    r.app.handle(msg(OWNER, OWNER, "again")).await;
    assert_eq!(texts(&r.tg).last().unwrap(), "fresh start");
}

#[tokio::test]
async fn the_run_loop_serves_telegram_updates_end_to_end() {
    let r = rig(&[OWNER]).await;
    r.tole
        .script_prompt(PromptStep::reply("end_turn", Some("pong")));
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let app = Arc::clone(&r.app);
    let source = r.adapter.clone();
    let runner = tokio::spawn(async move {
        app.run(&source, async {
            let _ = stop_rx.await;
        })
        .await;
    });
    r.tg.push_text(OWNER, OWNER, "ping");
    wait_until("the reply", || texts(&r.tg).iter().any(|t| t == "pong")).await;
    let _ = stop_tx.send(());
    tokio::time::timeout(Duration::from_secs(5), runner)
        .await
        .expect("the run loop stops on shutdown")
        .unwrap();
    // The bot token appears only in request paths, never in any message.
    assert_no_secrets(&r);
}
