//! The dispatcher: wires the chat channel, the tole serve client, the allowlist,
//! the session index and the approval buttons together.
//!
//! - An authorized text message becomes `POST /sessions/{id}/prompt`.
//! - `approval_required` becomes an approval prompt with Allow and Deny
//!   buttons; a press decides through `POST /approvals/{id}/decision`, the one
//!   gate. An unanswered approval is denied.
//! - After an allow the resumed turn has no HTTP answer, so the app polls
//!   `GET /approvals?session=` and `GET /sessions/{id}/status` until the
//!   session is idle and then reads the new log entries for the final answer.
//! - One turn at a time per chat. A message that arrives while a turn runs or
//!   an approval is pending is answered with a short notice, never queued.
//! - Chats that are not authorized get no reply at all, except that an unknown
//!   user who sends the pairing code is paired.
//! - Replies are short, secret-free sentences; message text is never logged.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tracing::{debug, info, warn};

use crate::approvals::{ApprovalButtons, Resolve};
use crate::auth::{Authorizer, PairOutcome, PairingCode, PairingCodeSource};
use crate::channel::{ChatId, ChatPort, ChatSource, Inbound, MessageRef, UserId};
use crate::config::Config;
use crate::error::{ChatError, ToleError};
use crate::sessions::{can_force_new, plan, Clock, NewReason, Plan, SessionIndex};
use crate::tole_client::ToleApi;
use crate::types::{ApprovalRow, ApprovalStatus, Decision, PromptOutcome};

const HELP: &str = "\
I pass your messages to tole and bring the answers back.\n\
\n\
/new      start a fresh session\n\
/cancel   stop the running turn, or deny a pending approval\n\
/status   show the state of the current session\n\
/help     this message\n\
\n\
When tole needs permission for an action I show Allow and Deny buttons. \
An approval you do not answer is denied.";

/// Timing knobs. The defaults suit production; tests shorten them.
#[derive(Debug, Clone)]
pub struct AppSettings {
    /// The tole profile every chat session opens under.
    pub profile: Option<String>,
    /// Working directory for new sessions.
    pub cwd: Option<String>,
    /// A new session starts after this long without an owner message.
    pub rollover: Duration,
    /// Poll interval while waiting for a resumed turn.
    pub poll_interval: Duration,
    /// Give up waiting for a resumed turn after this long.
    pub watch_timeout: Duration,
    /// After an allow, how long to wait for the resumed turn to show any sign
    /// of life before reporting that it finished without output.
    pub resume_grace: Duration,
    /// How often to refresh the typing indicator during a turn.
    pub typing_interval: Duration,
    /// How often to sweep unanswered approvals and refresh the pairing code.
    pub sweep_interval: Duration,
    /// How long a pairing code stays valid (must match the authorizer).
    pub pairing_ttl: Duration,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            profile: None,
            cwd: None,
            rollover: Duration::from_secs(4 * 3600),
            poll_interval: Duration::from_secs(1),
            watch_timeout: Duration::from_secs(30 * 60),
            resume_grace: Duration::from_secs(8),
            typing_interval: Duration::from_secs(4),
            sweep_interval: Duration::from_secs(30),
            pairing_ttl: Duration::from_secs(600),
        }
    }
}

impl AppSettings {
    /// Settings taken from a validated configuration.
    pub fn from_config(cfg: &Config) -> Self {
        Self {
            profile: cfg.tole.profile.clone(),
            cwd: cfg.tole.cwd.clone(),
            rollover: Duration::from_secs(cfg.sessions.rollover_hours.saturating_mul(3600)),
            pairing_ttl: Duration::from_secs(cfg.auth.pairing_ttl_secs),
            ..Self::default()
        }
    }
}

/// Where the one-time pairing code is shown to the operator (the console).
pub type Announce = Arc<dyn Fn(&PairingCode) + Send + Sync>;

struct Pending {
    approval_id: String,
    session_id: String,
    tool: String,
    message: MessageRef,
}

#[derive(Default)]
struct ChatRun {
    busy: bool,
    pending: Option<Pending>,
}

struct Inner<K: Clock, S: PairingCodeSource> {
    auth: Authorizer<K, S>,
    index: SessionIndex,
    buttons: ApprovalButtons,
    chats: HashMap<ChatId, ChatRun>,
    pairing_mode: bool,
    pairing_expires_ms: Option<u64>,
    /// Approvals whose denial could not be recorded; the next sweep retries.
    retry_deny: Vec<String>,
}

/// Most approvals kept for a retried denial.
const MAX_RETRY_DENY: usize = 16;
/// Most entry pages read at once when the resumed turn is idle.
const MAX_IDLE_PAGES: usize = 20;
/// Attempts at the baseline read before an allow.
const BASELINE_TRIES: usize = 3;

const DENY_NOT_RECORDED: &str =
    "I could not record the denial. tole will deny the request when it \
expires (15 minutes).";

/// What went wrong while serving a message.
enum Fail {
    Tole(ToleError),
    /// The session index could not be saved.
    State,
}

impl From<ToleError> for Fail {
    fn from(e: ToleError) -> Self {
        Fail::Tole(e)
    }
}

/// The gateway application, generic over the tole client and the chat port so
/// tests can drive it with fakes.
pub struct App<T, C, K: Clock, S: PairingCodeSource> {
    tole: T,
    chat: C,
    clock: K,
    settings: AppSettings,
    inner: Mutex<Inner<K, S>>,
    announce: Announce,
}

enum Command {
    New,
    Cancel,
    Status,
    Help,
    Unknown,
}

fn parse_command(text: &str) -> Option<Command> {
    let t = text.trim_start();
    if !t.starts_with('/') {
        return None;
    }
    let word = t.split_whitespace().next().unwrap_or("").to_lowercase();
    Some(match word.as_str() {
        "/new" => Command::New,
        "/cancel" => Command::Cancel,
        "/status" => Command::Status,
        "/help" | "/start" => Command::Help,
        _ => Command::Unknown,
    })
}

fn cap(text: &str, max: usize) -> String {
    let one: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() <= max {
        one
    } else {
        let cut: String = one.chars().take(max).collect();
        format!("{cut}...")
    }
}

fn reply_text(stop_reason: &str, text: Option<&str>) -> String {
    match text {
        Some(t) if !t.trim().is_empty() => t.to_owned(),
        _ => match stop_reason {
            "cancelled" => "Cancelled.".to_owned(),
            "end_turn" => "Done (no text answer).".to_owned(),
            other => format!("The turn stopped ({}) without an answer.", cap(other, 40)),
        },
    }
}

/// True when tole does not know the session. A prompt to a session the server
/// no longer holds (after a restart) is answered `500 {"error": "unknown
/// session: <id>"}` by tole 0.11, not 404, so that text counts too.
fn is_unknown_session(e: &ToleError) -> bool {
    match e {
        ToleError::NotFound(_) => true,
        ToleError::Server(text) => text.trim_start().starts_with("unknown session"),
        _ => false,
    }
}

/// Whether tole's answer to a decision says the decision took effect. An allow
/// must come back `approved`; a deny `denied` (or `expired`, which also counts
/// as denied).
fn decision_applied(decision: Decision, status: &str) -> bool {
    match decision {
        Decision::Allow => status == "approved",
        Decision::Deny => status == "denied" || status == "expired",
    }
}

fn tole_error_text(e: &ToleError) -> String {
    match e {
        ToleError::Unauthorized => {
            "The gateway is not authorized to use tole. Check the tole token in the gateway \
             config."
                .to_owned()
        }
        ToleError::Busy => {
            "tole is still working on the previous message. Send /cancel to stop it.".to_owned()
        }
        ToleError::Timeout => {
            "tole did not answer in time. The turn may still be running; check /status.".to_owned()
        }
        ToleError::Transport(_) => "Cannot reach tole. Is `tole serve` running?".to_owned(),
        ToleError::Unavailable(_) => {
            "tole is at capacity right now. Try again in a minute.".to_owned()
        }
        ToleError::NotFound(_) => "tole does not know that session any more. Send /new.".to_owned(),
        other => format!("tole reported a problem: {}", cap(&other.to_string(), 200)),
    }
}

impl<T, C, K, S> App<T, C, K, S>
where
    T: ToleApi + 'static,
    C: ChatPort + 'static,
    K: Clock + Clone + 'static,
    S: PairingCodeSource + 'static,
{
    /// Build the app. `pairing_mode` is true when the allowlist is empty: the
    /// app then announces a one-time pairing code through `announce`.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        tole: T,
        chat: C,
        clock: K,
        settings: AppSettings,
        auth: Authorizer<K, S>,
        index: SessionIndex,
        pairing_mode: bool,
        announce: Announce,
    ) -> Self {
        Self {
            tole,
            chat,
            clock,
            settings,
            inner: Mutex::new(Inner {
                auth,
                index,
                buttons: ApprovalButtons::new(),
                chats: HashMap::new(),
                pairing_mode,
                pairing_expires_ms: None,
                retry_deny: Vec::new(),
            }),
            announce,
        }
    }

    fn with<R>(&self, f: impl FnOnce(&mut Inner<K, S>) -> R) -> R {
        let mut guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        f(&mut guard)
    }

    async fn say(&self, chat: ChatId, text: &str) {
        if let Err(e) = self.chat.send_text(chat, text).await {
            warn!("could not send a message: {e}");
        }
    }

    async fn report(&self, chat: ChatId, fail: &Fail) {
        match fail {
            Fail::Tole(e) => {
                warn!("tole call failed: {e}");
                self.say(chat, &tole_error_text(e)).await;
            }
            Fail::State => {
                self.say(
                    chat,
                    "I could not save my session state, so I stopped. Check the gateway's state \
                     directory.",
                )
                .await;
            }
        }
    }

    /// Run until `shutdown` completes: poll the chat source, handle every event
    /// on its own task, and sweep unanswered approvals in the background.
    pub async fn run<Src: ChatSource>(
        self: Arc<Self>,
        source: &Src,
        shutdown: impl Future<Output = ()>,
    ) {
        self.ensure_pairing();
        let (stop_tx, mut stop_rx) = tokio::sync::watch::channel(false);
        let sweeper = {
            let app = Arc::clone(&self);
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(app.settings.sweep_interval);
                loop {
                    tokio::select! {
                        _ = tick.tick() => app.sweep().await,
                        _ = stop_rx.changed() => break,
                    }
                }
            })
        };
        tokio::pin!(shutdown);
        let mut backoff = Duration::from_secs(1);
        loop {
            tokio::select! {
                _ = &mut shutdown => {
                    info!("shutting down");
                    break;
                }
                batch = source.next() => match batch {
                    Ok(events) => {
                        backoff = Duration::from_secs(1);
                        for event in events {
                            let app = Arc::clone(&self);
                            tokio::spawn(async move { app.handle(event).await });
                        }
                    }
                    Err(e) => {
                        warn!("polling the chat platform failed: {e}");
                        let wait = match e {
                            ChatError::RateLimited { retry_after_secs } => {
                                Duration::from_secs(retry_after_secs.clamp(1, 120))
                            }
                            _ => backoff,
                        };
                        backoff = (backoff * 2).min(Duration::from_secs(30));
                        tokio::select! {
                            _ = &mut shutdown => break,
                            _ = tokio::time::sleep(wait) => {}
                        }
                    }
                }
            }
        }
        let _ = stop_tx.send(true);
        let _ = sweeper.await;
    }

    /// Handle one chat event.
    pub async fn handle(&self, event: Inbound) {
        match event {
            Inbound::Message {
                chat, user, text, ..
            } => self.on_message(chat, user, &text).await,
            Inbound::Callback {
                chat,
                user,
                callback_id,
                data,
                ..
            } => self.on_callback(chat, user, &callback_id, &data).await,
        }
    }

    /// Announce a pairing code when pairing is on and no valid code exists.
    pub fn ensure_pairing(&self) {
        let now = self.clock.now_ms();
        let ttl_ms = self.settings.pairing_ttl.as_millis() as u64;
        let code = self.with(|i| {
            if !i.pairing_mode || i.pairing_expires_ms.is_some_and(|e| now < e) {
                return None;
            }
            match i.auth.start_pairing() {
                Ok(code) => {
                    i.pairing_expires_ms = Some(now.saturating_add(ttl_ms));
                    Some(code)
                }
                Err(e) => {
                    warn!("cannot start pairing: {e}");
                    None
                }
            }
        });
        if let Some(code) = code {
            (self.announce)(&code);
        }
    }

    /// Deny through the gate. True when the denial is in place (recorded, or
    /// the approval is already gone); otherwise the id is kept for a retry.
    async fn deny_by_id(&self, id: &str) -> bool {
        match self.tole.decide(id, Decision::Deny).await {
            Ok(_) | Err(ToleError::NotFound(_)) => true,
            Err(e) => {
                warn!("could not deny an approval: {e}");
                self.with(|i| {
                    if !i.retry_deny.iter().any(|x| x == id) {
                        if i.retry_deny.len() >= MAX_RETRY_DENY {
                            i.retry_deny.remove(0);
                        }
                        i.retry_deny.push(id.to_owned());
                    }
                });
                false
            }
        }
    }

    /// Deny approvals nobody answered in time and refresh the pairing code.
    pub async fn sweep(&self) {
        let now = self.clock.now_ms();
        let retry = self.with(|i| std::mem::take(&mut i.retry_deny));
        for id in retry {
            self.deny_by_id(&id).await;
        }
        let expired = self.with(|i| i.buttons.expired_ids(now));
        for id in expired {
            let denied = self.deny_by_id(&id).await;
            let found = self.with(|i| {
                i.chats.iter_mut().find_map(|(chat, run)| {
                    run.pending
                        .take_if(|p| p.approval_id == id)
                        .map(|p| (*chat, p))
                })
            });
            if let Some((chat, p)) = found {
                let note = if denied {
                    format!("{}: not answered in time, denied.", cap(&p.tool, 60))
                } else {
                    format!("{}: not answered in time.", cap(&p.tool, 60))
                };
                if let Err(e) = self.chat.finish_approval(p.message, &note).await {
                    debug!("could not update an expired approval message: {e}");
                }
                if denied {
                    self.say(
                        chat,
                        "An approval was not answered in time and was denied. Send your message \
                         again if you still want it.",
                    )
                    .await;
                } else {
                    self.say(chat, DENY_NOT_RECORDED).await;
                }
            }
        }
        self.ensure_pairing();
    }

    async fn on_message(&self, chat: ChatId, user: UserId, text: &str) {
        if !self.with(|i| i.auth.is_authorized(user)) {
            self.try_pair(chat, user, text).await;
            return;
        }
        match parse_command(text) {
            Some(Command::New) => self.cmd_new(chat).await,
            Some(Command::Cancel) => self.cmd_cancel(chat).await,
            Some(Command::Status) => self.cmd_status(chat).await,
            Some(Command::Help) => self.say(chat, HELP).await,
            Some(Command::Unknown) => self.say(chat, "Unknown command. /help lists them.").await,
            None => self.on_text(chat, text).await,
        }
    }

    async fn try_pair(&self, chat: ChatId, user: UserId, text: &str) {
        let outcome = self.with(|i| {
            let o = i.auth.try_pair(user, text);
            if matches!(o, PairOutcome::Paired) {
                i.pairing_mode = false;
            }
            o
        });
        if matches!(outcome, PairOutcome::Paired) {
            info!("a user paired");
            let msg = format!(
                "Paired for this run. To keep access after a restart, add {} to allowed_user_ids \
                 in the gateway config.",
                user.0
            );
            self.say(chat, &msg).await;
        } else {
            debug!("dropped a message from an unauthorized user");
        }
    }

    async fn on_text(&self, chat: ChatId, text: &str) {
        enum Gate {
            Go,
            Busy,
            Pending(String),
        }
        let gate = self.with(|i| {
            let run = i.chats.entry(chat).or_default();
            if let Some(p) = &run.pending {
                Gate::Pending(p.tool.clone())
            } else if run.busy {
                Gate::Busy
            } else {
                run.busy = true;
                Gate::Go
            }
        });
        match gate {
            Gate::Busy => {
                self.say(
                    chat,
                    "Still working on the previous message. Send /cancel to stop it.",
                )
                .await
            }
            Gate::Pending(tool) => {
                let msg = format!(
                    "Waiting for your decision on {}. Press Allow or Deny, or send /cancel.",
                    cap(&tool, 60)
                );
                self.say(chat, &msg).await;
            }
            Gate::Go => {
                self.run_message(chat, text).await;
                self.with(|i| i.chats.entry(chat).or_default().busy = false);
            }
        }
    }

    async fn run_message(&self, chat: ChatId, text: &str) {
        let session_id = match self.ensure_session(chat).await {
            Ok(id) => id,
            Err(f) => return self.report(chat, &f).await,
        };
        let prompt = self.prompt_with_recovery(chat, &session_id, text);
        tokio::pin!(prompt);
        let mut tick = tokio::time::interval(self.settings.typing_interval);
        let result = loop {
            tokio::select! {
                r = &mut prompt => break r,
                _ = tick.tick() => {
                    if let Err(e) = self.chat.typing(chat).await {
                        debug!("typing indicator failed: {e}");
                    }
                }
            }
        };
        match result {
            Err(f) => self.report(chat, &f).await,
            Ok((sid, PromptOutcome::Reply { stop_reason, text })) => {
                debug!("turn on {sid} ended: {stop_reason}");
                self.say(chat, &reply_text(&stop_reason, text.as_deref()))
                    .await;
            }
            Ok((sid, PromptOutcome::ApprovalRequired { approval })) => {
                self.present_approval(chat, &sid, approval).await;
            }
        }
    }

    /// The session for this chat: reuse it, or open a new one when the rollover
    /// rule says so. The caller holds the chat's busy flag, so the plan sees
    /// no running turn and no pending approval.
    async fn ensure_session(&self, chat: ChatId) -> Result<String, Fail> {
        let now = self.clock.now_ms();
        let rollover_ms = self.settings.rollover.as_millis() as u64;
        let (planned, existing) = self.with(|i| {
            let existing = i.index.get(chat).map(|s| s.session_id.clone());
            let p = plan(i.index.get(chat), now, rollover_ms, false, false, false);
            (p, existing)
        });
        match (planned, existing) {
            (Plan::Reuse, Some(id)) => {
                let saved = self.with(|i| i.index.touch_owner_message(chat, now));
                if let Err(e) = saved {
                    warn!("cannot save the session index: {e}");
                    return Err(Fail::State);
                }
                Ok(id)
            }
            (Plan::NewSession { reason }, _) => {
                if matches!(reason, NewReason::Idle) {
                    self.say(
                        chat,
                        "Starting a new session after a long pause. Earlier context is not \
                         carried over.",
                    )
                    .await;
                }
                self.open_new(chat).await
            }
            (Plan::Reuse, None) => self.open_new(chat).await,
        }
    }

    async fn open_new(&self, chat: ChatId) -> Result<String, Fail> {
        let open = self
            .tole
            .create_session(
                self.settings.profile.as_deref(),
                self.settings.cwd.as_deref(),
            )
            .await?;
        let now = self.clock.now_ms();
        let id = open.session_id;
        let saved = self.with(|i| i.index.record_new_session(chat, &id, now));
        if let Err(e) = saved {
            warn!("cannot save the session index: {e}");
            return Err(Fail::State);
        }
        Ok(id)
    }

    /// Prompt; when tole no longer knows the session (it restarted), reopen it
    /// from its log, or start a fresh one when the log is gone too.
    async fn prompt_with_recovery(
        &self,
        chat: ChatId,
        session_id: &str,
        text: &str,
    ) -> Result<(String, PromptOutcome), Fail> {
        match self.tole.prompt(session_id, text).await {
            Err(e) if is_unknown_session(&e) => {
                let sid = match self
                    .tole
                    .resume_session(
                        session_id,
                        self.settings.profile.as_deref(),
                        self.settings.cwd.as_deref(),
                    )
                    .await
                {
                    Ok(open) => open.session_id,
                    Err(ToleError::NotFound(_)) => {
                        self.say(
                            chat,
                            "My previous session is gone, so I started a new one. Earlier context \
                             is not carried over.",
                        )
                        .await;
                        self.open_new(chat).await?
                    }
                    Err(e) => return Err(e.into()),
                };
                let out = self.tole.prompt(&sid, text).await?;
                Ok((sid, out))
            }
            Err(e) => Err(e.into()),
            Ok(out) => Ok((session_id.to_owned(), out)),
        }
    }

    async fn present_approval(&self, chat: ChatId, sid: &str, approval: Option<ApprovalRow>) {
        // Without a row the server says the request is already decided; an
        // unrelated pending row of the session is never shown in its place.
        match approval {
            Some(row) => self.show_approval(chat, sid, &row).await,
            None => {
                self.say(
                    chat,
                    "The approval request expired before I could show it. Send your message \
                     again.",
                )
                .await
            }
        }
    }

    /// Show the buttons. If they cannot be shown, deny: an approval nobody can
    /// answer must not stay open.
    async fn show_approval(&self, chat: ChatId, sid: &str, row: &ApprovalRow) {
        let now = self.clock.now_ms();
        let prompt = self.with(|i| i.buttons.prompt_for(chat, row, now));
        match self.chat.send_approval(chat, &prompt).await {
            Ok(message) => {
                self.with(|i| {
                    i.chats.entry(chat).or_default().pending = Some(Pending {
                        approval_id: row.id.clone(),
                        session_id: sid.to_owned(),
                        tool: row.tool.clone(),
                        message,
                    });
                });
            }
            Err(e) => {
                warn!("could not show an approval, denying it: {e}");
                if let Err(e) = self.tole.decide(&row.id, Decision::Deny).await {
                    warn!("could not deny the approval: {e}");
                }
            }
        }
    }

    async fn on_callback(&self, chat: ChatId, user: UserId, callback_id: &str, data: &str) {
        let answer = |text: Option<&'static str>| async move {
            if let Err(e) = self.chat.answer_callback(callback_id, text).await {
                debug!("could not answer a button press: {e}");
            }
        };
        if !self.with(|i| i.auth.is_authorized(user)) {
            return answer(None).await;
        }
        let now = self.clock.now_ms();
        match self.with(|i| i.buttons.resolve(chat, data, now)) {
            Resolve::Decide {
                approval_id,
                decision,
            } => {
                // The chat stays busy from the take until apply_decision ends,
                // so no message can slip in between the press and the watch.
                let taken = self.with(|i| {
                    let run = i.chats.get_mut(&chat)?;
                    let p = run.pending.take_if(|p| p.approval_id == approval_id)?;
                    run.busy = true;
                    Some(p)
                });
                let Some(p) = taken else {
                    return answer(Some("That request is no longer pending.")).await;
                };
                answer(Some(match decision {
                    Decision::Allow => "Allowed",
                    Decision::Deny => "Denied",
                }))
                .await;
                self.apply_decision(chat, p, decision).await;
            }
            Resolve::Expired { approval_id } => {
                answer(Some("This approval expired and was denied.")).await;
                self.deny_pending(chat, &approval_id).await;
            }
            Resolve::Unknown => answer(Some("This button is no longer valid.")).await,
            Resolve::WrongChat => answer(None).await,
        }
    }

    /// Deny the approval whose button was pressed after its window closed. The
    /// chat's pending entry is dropped only when it is that same approval.
    async fn deny_pending(&self, chat: ChatId, approval_id: &str) {
        let denied = self.deny_by_id(approval_id).await;
        let taken = self.with(|i| {
            i.chats
                .get_mut(&chat)
                .and_then(|r| r.pending.take_if(|p| p.approval_id == approval_id))
        });
        let Some(p) = taken else { return };
        let note = if denied {
            format!("{}: not answered in time, denied.", cap(&p.tool, 60))
        } else {
            format!("{}: not answered in time.", cap(&p.tool, 60))
        };
        if let Err(e) = self.chat.finish_approval(p.message, &note).await {
            debug!("could not update an expired approval message: {e}");
        }
        if denied {
            self.say(
                chat,
                "That approval was not answered in time and was denied. Send your message again \
                 if you still want it.",
            )
            .await;
        } else {
            self.say(chat, DENY_NOT_RECORDED).await;
        }
    }

    /// Apply a pressed decision. The caller set the chat busy; every exit
    /// clears it.
    async fn apply_decision(&self, chat: ChatId, p: Pending, decision: Decision) {
        self.apply_decision_inner(chat, p, decision).await;
        self.with(|i| i.chats.entry(chat).or_default().busy = false);
    }

    /// Ids of the session's pending approvals, best effort.
    async fn pending_ids(&self, sid: &str) -> Vec<String> {
        match self.tole.approvals(Some(sid)).await {
            Ok(rows) => rows
                .into_iter()
                .filter(|r| r.status == ApprovalStatus::Pending)
                .map(|r| r.id)
                .collect(),
            Err(e) => {
                debug!("could not read the open approvals: {e}");
                Vec::new()
            }
        }
    }

    async fn apply_decision_inner(&self, chat: ChatId, p: Pending, decision: Decision) {
        // The log position and the open approvals before the decision: an
        // allow resumes the turn, and the answer is whatever the assistant
        // writes after this point; rows already open are not its requests.
        let mut baseline = None;
        let mut ignore = vec![p.approval_id.clone()];
        if decision == Decision::Allow {
            for attempt in 0..BASELINE_TRIES {
                if attempt > 0 {
                    tokio::time::sleep(self.settings.poll_interval).await;
                }
                match self.tole.entries(&p.session_id, 0, 1).await {
                    Ok(page) => {
                        baseline = Some(page.last_seq);
                        break;
                    }
                    Err(e) => debug!("baseline read failed: {e}"),
                }
            }
            ignore.extend(self.pending_ids(&p.session_id).await);
        }
        let label = match decision {
            Decision::Allow => "allowed",
            Decision::Deny => "denied",
        };
        match self.tole.decide(&p.approval_id, decision).await {
            Ok(ack) if !decision_applied(decision, &ack.status) => {
                // tole answers with the row's existing status when the request
                // had already closed (expired or decided elsewhere): nothing ran.
                let status = cap(&ack.status, 20);
                let note = format!("{}: already {status}.", cap(&p.tool, 60));
                let _ = self.chat.finish_approval(p.message, &note).await;
                let text = format!(
                    "That approval was already {status} when your decision arrived, so nothing \
                     was changed. Send your message again if you still want it."
                );
                self.say(chat, &text).await;
            }
            Ok(_) => {
                let note = format!("{}: {label}.", cap(&p.tool, 60));
                if let Err(e) = self.chat.finish_approval(p.message, &note).await {
                    debug!("could not update the approval message: {e}");
                }
                match decision {
                    Decision::Deny => {
                        self.say(chat, "Denied. That action will not run.").await;
                    }
                    Decision::Allow => {
                        self.watch_resume(chat, &p.session_id, baseline, &ignore)
                            .await;
                    }
                }
            }
            Err(ToleError::NotFound(_)) => {
                let note = format!("{}: no longer pending.", cap(&p.tool, 60));
                let _ = self.chat.finish_approval(p.message, &note).await;
                self.say(chat, "That approval is gone (it may have expired).")
                    .await;
            }
            Err(e) => {
                warn!("could not record the decision: {e}");
                if decision == Decision::Allow && e.is_retryable() {
                    self.after_ambiguous_allow(chat, p, baseline, &ignore).await;
                } else {
                    let note = format!("{}: not recorded.", cap(&p.tool, 60));
                    let _ = self.chat.finish_approval(p.message, &note).await;
                    let text = if decision == Decision::Deny {
                        DENY_NOT_RECORDED
                    } else {
                        "I could not record your decision. The request stays unanswered and \
                         will be denied when it expires."
                    };
                    self.say(chat, text).await;
                    if decision == Decision::Deny {
                        self.with(|i| {
                            if i.retry_deny.len() < MAX_RETRY_DENY
                                && !i.retry_deny.contains(&p.approval_id)
                            {
                                i.retry_deny.push(p.approval_id.clone());
                            }
                        });
                    }
                }
            }
        }
    }

    /// The allow call failed in a way that may still have been applied: read
    /// the approval back to learn the real state.
    async fn after_ambiguous_allow(
        &self,
        chat: ChatId,
        p: Pending,
        baseline: Option<u64>,
        ignore: &[String],
    ) {
        let state = match self.tole.approvals(Some(&p.session_id)).await {
            Ok(rows) => rows
                .into_iter()
                .find(|r| r.id == p.approval_id)
                .map(|r| (r.status == ApprovalStatus::Approved, r)),
            Err(e) => {
                debug!("could not re-read the approval: {e}");
                None
            }
        };
        match state {
            Some((true, _)) => {
                let note = format!("{}: allowed.", cap(&p.tool, 60));
                let _ = self.chat.finish_approval(p.message, &note).await;
                self.watch_resume(chat, &p.session_id, baseline, ignore)
                    .await;
            }
            Some((false, row)) if row.status == ApprovalStatus::Pending => {
                let note = format!("{}: not recorded.", cap(&p.tool, 60));
                let _ = self.chat.finish_approval(p.message, &note).await;
                self.say(
                    chat,
                    "Your Allow was not recorded. Press Allow or Deny again.",
                )
                .await;
                self.show_approval(chat, &p.session_id, &row).await;
            }
            _ => {
                let note = format!("{}: state unknown.", cap(&p.tool, 60));
                let _ = self.chat.finish_approval(p.message, &note).await;
                self.say(
                    chat,
                    "I could not tell whether your Allow was recorded. The state is unknown; \
                     check /status.",
                )
                .await;
            }
        }
    }

    /// After an allow: wait for the resumed turn, then deliver its answer, or
    /// show the next approval if the turn stopped on one.
    async fn watch_resume(
        &self,
        chat: ChatId,
        sid: &str,
        baseline: Option<u64>,
        ignore: &[String],
    ) {
        let started = Instant::now();
        let mut cursor = baseline;
        let mut seen_busy = false;
        let mut last_text: Option<String> = None;
        loop {
            tokio::time::sleep(self.settings.poll_interval).await;
            match self.next_pending(sid, ignore).await {
                Ok(Some(row)) => return self.show_approval(chat, sid, &row).await,
                Ok(None) => {}
                Err(e) if e.is_retryable() => debug!("transient error while watching: {e}"),
                Err(e) => return self.report(chat, &Fail::Tole(e)).await,
            }
            // Status first, then the log: when the status says idle, the log
            // read that follows already holds the final answer.
            let busy = match self.tole.status(sid).await {
                Ok(st) => st.busy,
                Err(e) if e.is_retryable() => {
                    debug!("transient error while watching: {e}");
                    true
                }
                Err(e) => return self.report(chat, &Fail::Tole(e)).await,
            };
            if let Some(c) = cursor {
                // While busy one page per poll is enough; once idle, read on
                // until the end so the last answer is not a mid-turn one.
                let mut at = c;
                for _ in 0..MAX_IDLE_PAGES {
                    match self.tole.entries(sid, at, 100).await {
                        Ok(page) => {
                            for e in &page.entries {
                                if let Some(("assistant", t)) = e.message() {
                                    last_text = Some(t.to_owned());
                                }
                            }
                            let next = page.next_after.max(at);
                            let more = page.has_more && next > at;
                            at = next;
                            cursor = Some(at);
                            if busy || !more {
                                break;
                            }
                        }
                        Err(e) if e.is_retryable() => {
                            debug!("transient error while reading: {e}");
                            break;
                        }
                        Err(e) => return self.report(chat, &Fail::Tole(e)).await,
                    }
                }
            }
            if busy {
                seen_busy = true;
            } else if last_text.is_some()
                || seen_busy
                || started.elapsed() >= self.settings.resume_grace
            {
                // The turn may have parked on another approval in the instant
                // between the two reads above.
                if let Ok(Some(row)) = self.next_pending(sid, ignore).await {
                    return self.show_approval(chat, sid, &row).await;
                }
                let msg = match (last_text, cursor) {
                    (Some(t), _) => t,
                    (None, None) => "The action was allowed and is running or done, but I could \
                                     not read its answer. Use /status."
                        .to_owned(),
                    (None, Some(_)) => "The action finished without a text answer.".to_owned(),
                };
                return self.say(chat, &msg).await;
            }
            if started.elapsed() >= self.settings.watch_timeout {
                return self
                    .say(
                        chat,
                        "No result yet after a long wait. Check /status; the turn may still be \
                         running.",
                    )
                    .await;
            }
        }
    }

    /// The first pending approval of the session that is not in `ignore`.
    async fn next_pending(
        &self,
        sid: &str,
        ignore: &[String],
    ) -> Result<Option<ApprovalRow>, ToleError> {
        Ok(self
            .tole
            .approvals(Some(sid))
            .await?
            .into_iter()
            .find(|r| r.status == ApprovalStatus::Pending && !ignore.contains(&r.id)))
    }

    async fn cmd_new(&self, chat: ChatId) {
        let allowed = self.with(|i| {
            let run = i.chats.entry(chat).or_default();
            if can_force_new(run.busy || run.pending.is_some()) {
                run.busy = true;
                true
            } else {
                false
            }
        });
        if !allowed {
            return self
                .say(
                    chat,
                    "Finish or /cancel the running work before starting a new session.",
                )
                .await;
        }
        let result = self.open_new(chat).await;
        self.with(|i| i.chats.entry(chat).or_default().busy = false);
        match result {
            Ok(_) => self.say(chat, "Started a new session.").await,
            Err(f) => self.report(chat, &f).await,
        }
    }

    async fn cmd_cancel(&self, chat: ChatId) {
        let (session, pending) = self.with(|i| {
            (
                i.index.get(chat).map(|s| s.session_id.clone()),
                i.chats.get_mut(&chat).and_then(|r| r.pending.take()),
            )
        });
        if let Some(p) = pending {
            let denied = self.deny_by_id(&p.approval_id).await;
            let note = if denied {
                format!("{}: denied (cancelled).", cap(&p.tool, 60))
            } else {
                format!("{}: cancelled.", cap(&p.tool, 60))
            };
            let _ = self.chat.finish_approval(p.message, &note).await;
            if denied {
                return self
                    .say(chat, "Cancelled. The pending action was denied.")
                    .await;
            }
            return self.say(chat, DENY_NOT_RECORDED).await;
        }
        match session {
            None => self.say(chat, "Nothing to cancel.").await,
            Some(sid) => match self.tole.cancel(&sid).await {
                Ok(()) => self.say(chat, "Cancel requested.").await,
                Err(ToleError::NotFound(_)) => self.say(chat, "Nothing is running.").await,
                Err(e) => self.report(chat, &Fail::Tole(e)).await,
            },
        }
    }

    async fn cmd_status(&self, chat: ChatId) {
        let (session, pending) = self.with(|i| {
            (
                i.index.get(chat).map(|s| s.session_id.clone()),
                i.chats
                    .get(&chat)
                    .and_then(|r| r.pending.as_ref().map(|p| p.tool.clone())),
            )
        });
        let Some(sid) = session else {
            return self
                .say(chat, "No session yet. Send a message to start one.")
                .await;
        };
        match self.tole.status(&sid).await {
            Ok(st) => {
                let entries = st
                    .entries
                    .map_or_else(|| "unknown".to_owned(), |n| n.to_string());
                let mut msg = format!(
                    "Session {sid}: {} ({entries} entries).",
                    if st.busy { "working" } else { "idle" }
                );
                if let Some(tool) = pending {
                    msg.push_str(&format!(
                        " Waiting for your decision on {}.",
                        cap(&tool, 60)
                    ));
                }
                self.say(chat, &msg).await;
            }
            Err(e) => self.report(chat, &Fail::Tole(e)).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_matching_status_counts_as_applied() {
        assert!(decision_applied(Decision::Allow, "approved"));
        assert!(!decision_applied(Decision::Allow, "expired"));
        assert!(!decision_applied(Decision::Allow, "denied"));
        assert!(decision_applied(Decision::Deny, "denied"));
        assert!(decision_applied(Decision::Deny, "expired"));
        assert!(!decision_applied(Decision::Deny, "approved"));
    }

    #[test]
    fn commands_are_recognized() {
        assert!(matches!(parse_command("/new"), Some(Command::New)));
        assert!(matches!(
            parse_command("  /CANCEL now"),
            Some(Command::Cancel)
        ));
        assert!(matches!(parse_command("/start"), Some(Command::Help)));
        assert!(matches!(parse_command("/nope"), Some(Command::Unknown)));
        assert!(parse_command("hello /new").is_none());
    }

    #[test]
    fn unknown_session_covers_the_real_servers_500() {
        assert!(is_unknown_session(&ToleError::NotFound("x".into())));
        assert!(is_unknown_session(&ToleError::Server(
            "unknown session: srv-1".into()
        )));
        assert!(!is_unknown_session(&ToleError::Server("boom".into())));
        assert!(!is_unknown_session(&ToleError::Busy));
    }

    #[test]
    fn reply_text_falls_back_by_stop_reason() {
        assert_eq!(reply_text("end_turn", Some("hi")), "hi");
        assert_eq!(reply_text("cancelled", None), "Cancelled.");
        assert_eq!(reply_text("end_turn", Some("  ")), "Done (no text answer).");
        assert!(reply_text("refusal", None).contains("refusal"));
    }

    #[test]
    fn cap_collapses_whitespace_and_truncates_on_char_boundaries() {
        assert_eq!(cap("a \n b", 10), "a b");
        assert_eq!(cap("ééééé", 3), "ééé...");
    }

    #[test]
    fn error_text_never_echoes_transport_details() {
        let t = tole_error_text(&ToleError::Transport("http://x/secret".into()));
        assert!(!t.contains("secret"));
    }
}
