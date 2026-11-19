//! What verified deliveries leave behind for the app: approvals tole announced
//! (so a watcher need not poll for them) and which approvals already closed.
//!
//! Bounded and in memory. Nothing here is ever an authority: a notified
//! approval is shown with the same buttons as a polled one and is decided only
//! through tole's one gate, so a lost or reordered event costs a poll, not
//! safety.

use std::collections::VecDeque;
use std::sync::Mutex;

use tokio::sync::watch;

use crate::types::{ApprovalRow, ApprovalStatus};

const MAX_REQUESTED: usize = 64;
const MAX_RESOLVED: usize = 256;
/// Longest approval life tole uses (`EXPIRY_SECS`).
const MAX_LIFE_SECS: u64 = 900;

/// An `approval_requested` event after verification and field caps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requested {
    /// The approval id.
    pub approval_id: String,
    /// The tole session that parked.
    pub session_id: String,
    /// The tool name.
    pub tool: String,
    /// The redacted one-line description (plain text, untrusted).
    pub summary: String,
    /// Unix seconds, as tole stamped it.
    pub created_at: u64,
    /// Unix seconds.
    pub expires_at: u64,
}

#[derive(Default)]
struct State {
    requested: VecDeque<Requested>,
    resolved: VecDeque<String>,
}

/// Shared between the receiver (writer) and the app (reader).
pub struct NotifyHub {
    state: Mutex<State>,
    tx: watch::Sender<u64>,
    fallback_poll: std::time::Duration,
}

impl NotifyHub {
    /// A hub; `fallback_poll` is how often a watcher still polls approvals.
    pub fn new(fallback_poll: std::time::Duration) -> Self {
        Self {
            state: Mutex::new(State::default()),
            tx: watch::channel(0).0,
            fallback_poll,
        }
    }

    /// The approval poll cadence while the receiver is on.
    pub fn fallback_poll(&self) -> std::time::Duration {
        self.fallback_poll
    }

    /// A receiver that changes whenever an event is recorded.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.tx.subscribe()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn bump(&self) {
        self.tx.send_modify(|n| *n = n.wrapping_add(1));
    }

    /// Record an announced approval (unless it already closed).
    pub fn record_requested(&self, r: Requested) {
        {
            let mut s = self.lock();
            if s.resolved.contains(&r.approval_id)
                || s.requested.iter().any(|x| x.approval_id == r.approval_id)
            {
                return;
            }
            if s.requested.len() >= MAX_REQUESTED {
                s.requested.pop_front();
            }
            s.requested.push_back(r);
        }
        self.bump();
    }

    /// Record that an approval left `Pending` (any outcome). Events carry no
    /// order, so this also cancels a `requested` that arrives later.
    pub fn record_resolved(&self, approval_id: &str) {
        {
            let mut s = self.lock();
            s.requested.retain(|x| x.approval_id != approval_id);
            if !s.resolved.iter().any(|x| x == approval_id) {
                if s.resolved.len() >= MAX_RESOLVED {
                    s.resolved.pop_front();
                }
                s.resolved.push_back(approval_id.to_owned());
            }
        }
        self.bump();
    }

    /// Take the first announced approval of `session` not in `ignore`, as a
    /// row the app can show. Entries past their expiry are dropped.
    pub fn take_requested(
        &self,
        session: &str,
        ignore: &[String],
        now_ms: u64,
    ) -> Option<ApprovalRow> {
        let now_s = now_ms / 1000;
        let mut s = self.lock();
        s.requested
            .retain(|x| x.expires_at == 0 || x.expires_at > now_s);
        let pos = s
            .requested
            .iter()
            .position(|x| x.session_id == session && !ignore.contains(&x.approval_id))?;
        let r = s.requested.remove(pos)?;
        let life = r
            .expires_at
            .saturating_sub(r.created_at)
            .clamp(1, MAX_LIFE_SECS);
        let created_ms = r.created_at.saturating_mul(1000);
        // A stamp from the future or the far past is not trusted for timing.
        let created_ms = if created_ms <= now_ms && now_ms - created_ms < MAX_LIFE_SECS * 1000 {
            created_ms
        } else {
            now_ms
        };
        Some(ApprovalRow {
            id: r.approval_id,
            session_id: r.session_id,
            tool: r.tool,
            description: r.summary,
            status: ApprovalStatus::Pending,
            created_at_ms: created_ms,
            expires_in_seconds: life,
        })
    }

    /// True when the approval was reported closed.
    pub fn is_resolved(&self, approval_id: &str) -> bool {
        self.lock().resolved.iter().any(|x| x == approval_id)
    }
}
