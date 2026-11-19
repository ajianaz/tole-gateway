//! The `turn_finished` event (tole `docs/turn-finished-event.md`, #962).
//!
//! tole sends one per settled `tole serve` turn: the session id, a closed
//! status word, the log position `last_seq` and, where it applies, the approval
//! the turn stopped on or a fixed reason code. It carries no text, so it is a
//! **hint**: the app still reads the answer through the authenticated serve
//! API, after its own cursor. Parsing here is strict about ids and lenient about
//! vocabulary: a status or code word added later by tole is kept as `Other`
//! rather than refused, because a refusal would only make tole retry it.

use ring::digest;
use serde_json::Value;

use super::verify::valid_token;

/// Largest `turn_finished` body accepted. tole refuses to send one over 1 KiB
/// (`MAX_TURN_BODY`); the margin is for an additive field.
pub const MAX_TURN_BODY: usize = 2048;

/// How the turn ended (`data.status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnStatus {
    /// `completed`: the answer is in the log.
    Completed,
    /// `failed`, with a [`TurnCode`].
    Failed,
    /// `cancelled` (tole folds an owner detach into it by default).
    Cancelled,
    /// `detached`, when tole reports it separately.
    Detached,
    /// `budget_exhausted`.
    BudgetExhausted,
    /// `blocked`, with a [`TurnCode`].
    Blocked,
    /// `approval_pending`: the turn stopped on the approval it names.
    ApprovalPending,
    /// A word this gateway does not know yet.
    Other,
}

impl TurnStatus {
    fn parse(s: &str) -> Self {
        match s {
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            "detached" => Self::Detached,
            "budget_exhausted" => Self::BudgetExhausted,
            "blocked" => Self::Blocked,
            "approval_pending" => Self::ApprovalPending,
            _ => Self::Other,
        }
    }
}

/// The fixed reason of a `failed` or `blocked` turn (`data.code`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnCode {
    /// `provider`: the model provider failed.
    Provider,
    /// `storage`: the session log could not be written.
    Storage,
    /// `loop`: the turn repeated a tool call.
    Loop,
    /// `unknown_tool`: the model asked for a tool that does not exist.
    UnknownTool,
    /// `stop_gate`: a stop gate refused to end the turn.
    StopGate,
    /// `internal`: any other failure.
    Internal,
    /// `approval`: a call needed an approval but none was queued.
    Approval,
    /// A word this gateway does not know yet.
    Other,
}

impl TurnCode {
    fn parse(s: &str) -> Self {
        match s {
            "provider" => Self::Provider,
            "storage" => Self::Storage,
            "loop" => Self::Loop,
            "unknown_tool" => Self::UnknownTool,
            "stop_gate" => Self::StopGate,
            "internal" => Self::Internal,
            "approval" => Self::Approval,
            _ => Self::Other,
        }
    }
}

/// A verified, parsed `turn_finished` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnFinished {
    /// The serve session whose turn ended.
    pub session_id: String,
    /// How it ended.
    pub status: TurnStatus,
    /// The reason word, when tole sent one.
    pub code: Option<TurnCode>,
    /// The `seq` of the newest log entry when the turn settled.
    pub last_seq: u64,
    /// When tole built the event (envelope `at`, unix seconds; retries keep
    /// it). Orders two settles at the same `last_seq`, also across a restart.
    pub at: u64,
    /// tole's per-process settle counter, from the event id (0 when absent).
    /// It restarts with tole, so it only breaks a tie of equal `at`.
    pub settle: u64,
    /// The approval an `approval_pending` turn stopped on.
    pub approval_id: Option<String>,
}

impl TurnFinished {
    /// The order of two events of one session: by `last_seq` (durable, the
    /// only order tole keeps across a restart), then `at`, then `settle`.
    /// `settle` alone is not trusted: after a tole restart it starts at 1
    /// again, while a later settle at the same `last_seq` has a later `at`.
    pub fn position(&self) -> (u64, u64, u64) {
        (self.last_seq, self.at, self.settle)
    }
}

/// tole's session id rule: `[A-Za-z0-9_-]`, 1 to 64 bytes.
fn valid_session_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}

/// The first 16 hex digits of SHA-256 over `s` (tole's `short_digest`).
fn short_digest(s: &str) -> String {
    let d = digest::digest(&digest::SHA256, s.as_bytes());
    d.as_ref()[..8].iter().map(|b| format!("{b:02x}")).collect()
}

fn number(s: &str) -> Option<u64> {
    if s.is_empty() || s.len() > 20 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// Split `turn:<readable>:<hash16>:<last_seq>[:<settle>]` and check that it
/// names `session_id`: the readable part and the hash must both match it. Returns
/// `(last_seq, settle)`.
fn parse_id(id: &str, session_id: &str) -> Option<(u64, u64)> {
    let parts: Vec<&str> = id.split(':').collect();
    let (hash, seq, settle) = match parts.as_slice() {
        ["turn", readable, hash, seq] if *readable == session_id => (*hash, *seq, None),
        ["turn", readable, hash, seq, settle] if *readable == session_id => {
            (*hash, *seq, Some(*settle))
        }
        _ => return None,
    };
    // A valid session id is its own readable part (nothing to replace, at
    // most 64 bytes), and the hash binds the id to it.
    if hash != short_digest(session_id) {
        return None;
    }
    let settle = match settle {
        Some(s) => number(s)?,
        None => 0,
    };
    Some((number(seq)?, settle))
}

/// Parse the `data` of a `turn_finished` whose envelope `id` already matched
/// the signed headers; `at` is the envelope's `at`. `None` when a field is missing or not well formed, or
/// the id does not belong to the session and position the data names.
pub fn parse(id: &str, at: u64, data: &Value) -> Option<TurnFinished> {
    let session_id = data.get("session_id")?.as_str()?;
    if !valid_session_id(session_id) {
        return None;
    }
    let status = TurnStatus::parse(data.get("status")?.as_str()?);
    let last_seq = data.get("last_seq")?.as_u64()?;
    let (id_seq, settle) = parse_id(id, session_id)?;
    if id_seq != last_seq {
        return None;
    }
    let approval_id = match data.get("approval_id") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_str().filter(|s| valid_token(s, 128))?.to_owned()),
    };
    let code = match data.get("code") {
        None | Some(Value::Null) => None,
        Some(v) => Some(TurnCode::parse(v.as_str()?)),
    };
    Some(TurnFinished {
        session_id: session_id.to_owned(),
        status,
        code,
        last_seq,
        at,
        settle,
        approval_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // tole's second pinned vector (`webhook_out/turn_tests.rs`).
    const ID: &str = "turn:s-1f2e3d:357319c7c674b505:14:1";

    #[test]
    fn the_digest_matches_tole() {
        assert_eq!(short_digest("s-1f2e3d"), "357319c7c674b505");
    }

    #[test]
    fn the_pinned_body_parses() {
        let data = json!({"last_seq": 14, "session_id": "s-1f2e3d", "status": "completed"});
        let t = parse(ID, 7, &data).unwrap();
        assert_eq!(t.session_id, "s-1f2e3d");
        assert_eq!(t.status, TurnStatus::Completed);
        assert_eq!(t.position(), (14, 7, 1));
        assert_eq!((t.code, t.approval_id), (None, None));
    }

    #[test]
    fn an_id_without_a_settle_counter_is_settle_zero() {
        let data = json!({"last_seq": 14, "session_id": "s-1f2e3d", "status": "cancelled"});
        let t = parse("turn:s-1f2e3d:357319c7c674b505:14", 7, &data).unwrap();
        assert_eq!(t.position(), (14, 7, 0));
    }

    #[test]
    fn the_id_must_name_the_same_session_and_position() {
        let ok = json!({"last_seq": 14, "session_id": "s-1f2e3d", "status": "completed"});
        let other = json!({"last_seq": 14, "session_id": "s-other", "status": "completed"});
        let moved = json!({"last_seq": 15, "session_id": "s-1f2e3d", "status": "completed"});
        assert!(parse(ID, 7, &other).is_none());
        assert!(parse(ID, 7, &moved).is_none());
        for id in [
            "turn:s-1f2e3d:0000000000000000:14:1",
            "turn:s-1f2e3e:357319c7c674b505:14:1",
            "cron:s-1f2e3d:357319c7c674b505:14:1",
            "turn:s-1f2e3d:357319c7c674b505:14:1:2",
            "turn:s-1f2e3d:357319c7c674b505:x:1",
            "turn:s-1f2e3d:357319c7c674b505:14:-1",
        ] {
            assert!(parse(id, 7, &ok).is_none(), "{id}");
        }
    }

    #[test]
    fn fields_are_checked_and_new_words_are_kept_as_other() {
        let base = |extra: Value| {
            let mut d = json!({"last_seq": 14, "session_id": "s-1f2e3d", "status": "blocked"});
            for (k, v) in extra.as_object().unwrap() {
                d[k] = v.clone();
            }
            parse(ID, 7, &d)
        };
        assert_eq!(
            base(json!({"code": "stop_gate"})).unwrap().code,
            Some(TurnCode::StopGate)
        );
        assert_eq!(
            base(json!({"code": "something_new"})).unwrap().code,
            Some(TurnCode::Other)
        );
        assert_eq!(
            base(json!({"status": "paused"})).unwrap().status,
            TurnStatus::Other
        );
        assert_eq!(
            base(json!({"approval_id": "apr-1"})).unwrap().approval_id,
            Some("apr-1".into())
        );
        assert!(base(json!({"approval_id": "a b"})).is_none());
        assert!(base(json!({"code": 3})).is_none());
        assert!(base(json!({"status": 1})).is_none());
        assert!(base(json!({"last_seq": -1})).is_none());
        assert!(base(json!({"session_id": "s/1"})).is_none());
    }
}
