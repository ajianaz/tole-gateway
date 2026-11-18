//! Types shared by the tole serve client and the rest of the gateway.
//!
//! They mirror the serve wire format documented in `docs/mobile-control.md` of
//! the tole repository. Clients must ignore unknown fields, so none of these
//! types uses `deny_unknown_fields`.

use serde::Deserialize;
use serde_json::Value;

/// Status of a queued approval, as `GET /approvals` reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApprovalStatus {
    /// Waiting for a decision.
    Pending,
    /// Allowed (one shot for exactly that tool call).
    Approved,
    /// Denied.
    Denied,
    /// Not decided within the server's window (900 s): counts as denied.
    Expired,
    /// A status this client does not know.
    #[serde(other)]
    Unknown,
}

/// One row of `GET /approvals`. There is no risk-tier field: over serve only
/// Write tools can be queued, Destructive tools are never callable.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalRow {
    /// The approval id, the argument of `POST /approvals/{id}/decision`.
    pub id: String,
    /// The serve session that queued it.
    #[serde(default)]
    pub session_id: String,
    /// The tool name.
    pub tool: String,
    /// One sanitized line describing the call (redacted, not length-capped).
    #[serde(default)]
    pub description: String,
    /// Current status.
    pub status: ApprovalStatus,
    /// Queue time, milliseconds since the Unix epoch.
    #[serde(default)]
    pub created_at_ms: u64,
    /// The server's expiry window in seconds.
    #[serde(default)]
    pub expires_in_seconds: u64,
}

/// A decision on an approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Allow exactly that call once.
    Allow,
    /// Deny it.
    Deny,
}

impl Decision {
    /// The wire value of `{"decision": ...}`.
    pub fn as_str(self) -> &'static str {
        match self {
            Decision::Allow => "allow",
            Decision::Deny => "deny",
        }
    }
}

/// The answer of `POST /approvals/{id}/decision`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DecisionAck {
    /// The approval id.
    pub id: String,
    /// `approved` or `denied` (see [`ApprovalStatus`]).
    pub status: String,
    /// True (HTTP 202) when an allow started a resume that has no HTTP answer:
    /// the result must be read from the session log.
    #[serde(default)]
    pub resuming: bool,
}

/// The answer of `POST /sessions/{id}/prompt` that the gateway cares about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptOutcome {
    /// The turn ended. `text` is the final answer when there is one.
    Reply {
        /// `end_turn`, `cancelled`, `refusal`, ...
        stop_reason: String,
        /// The final assistant text, when the turn produced one.
        text: Option<String>,
    },
    /// The turn stopped waiting for an approval. `approval` is omitted by the
    /// server when that entry expired or was decided before the lookup; use
    /// `GET /approvals?session=` then.
    ApprovalRequired {
        /// The pending row, when still pending.
        approval: Option<ApprovalRow>,
    },
}

/// The answer of `POST /sessions` (create or resume).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionOpen {
    /// The session id.
    pub session_id: String,
    /// The workspace directory, when reported.
    #[serde(default)]
    pub workspace: Option<String>,
    /// True when an existing session was reopened.
    #[serde(default)]
    pub resumed: bool,
    /// True when the session was already live (nothing reopened).
    #[serde(default)]
    pub already_live: bool,
    /// Whether a turn is running, reported for an already-live session.
    #[serde(default)]
    pub busy: Option<bool>,
    /// The profile the session runs under, when one was used.
    #[serde(default)]
    pub profile: Option<String>,
}

/// The part of `GET /sessions/{id}/status` the gateway uses.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SessionStatus {
    /// The session id.
    #[serde(default)]
    pub id: String,
    /// Entry count; `None` while a turn is in flight.
    #[serde(default)]
    pub entries: Option<u64>,
    /// True while a turn is running.
    #[serde(default)]
    pub busy: bool,
}

/// One entry of a session log, exactly as `GET .../entries` returns it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    /// Strictly increasing, not dense: never assume `+1`.
    pub seq: u64,
    /// The entry id.
    #[serde(default)]
    pub id: String,
    /// The parent entry id.
    #[serde(default)]
    pub parent_id: Option<String>,
    /// The entry type, for example `message`.
    #[serde(rename = "type")]
    pub kind: String,
    /// Milliseconds since the Unix epoch.
    #[serde(default)]
    pub timestamp: Option<u64>,
    /// The entry payload; for a message `{"role": "...", "text": "..."}`.
    #[serde(default)]
    pub payload: Value,
}

impl Entry {
    /// `(role, text)` when this is a message entry with both fields.
    pub fn message(&self) -> Option<(&str, &str)> {
        if self.kind != "message" {
            return None;
        }
        let role = self.payload.get("role")?.as_str()?;
        let text = self.payload.get("text")?.as_str()?;
        Some((role, text))
    }
}

/// One page of `GET /sessions/{id}/entries?after=<seq>&limit=<n>`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntriesPage {
    /// The entries with `seq > after`.
    #[serde(default)]
    pub entries: Vec<Entry>,
    /// Pass this as `after` for the next page.
    #[serde(default)]
    pub next_after: u64,
    /// True when more pages follow.
    #[serde(default)]
    pub has_more: bool,
    /// The highest sequence number in the log.
    #[serde(default)]
    pub last_seq: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_an_approval_row() {
        let row: ApprovalRow = serde_json::from_str(
            r#"{"id":"apr-1","sessionId":"srv-9","tool":"write_file",
                "description":"write notes.md","status":"pending",
                "createdAtMs":1787807227226,"expiresInSeconds":900,"extra":1}"#,
        )
        .unwrap();
        assert_eq!(row.id, "apr-1");
        assert_eq!(row.session_id, "srv-9");
        assert_eq!(row.status, ApprovalStatus::Pending);
        assert_eq!(row.expires_in_seconds, 900);
    }

    #[test]
    fn unknown_status_does_not_fail() {
        let row: ApprovalRow =
            serde_json::from_str(r#"{"id":"a","tool":"t","status":"weird"}"#).unwrap();
        assert_eq!(row.status, ApprovalStatus::Unknown);
    }

    #[test]
    fn decodes_an_entries_page_from_the_documented_example() {
        let page: EntriesPage = serde_json::from_str(
            r#"{"id":"srv-1","entries":[{"seq":1,"id":"e_1","parentId":null,
                "type":"message","timestamp":1787807227226,
                "payload":{"role":"user","text":"hi"}}],
                "nextAfter":1,"hasMore":false,"lastSeq":1}"#,
        )
        .unwrap();
        assert_eq!(page.entries.len(), 1);
        assert_eq!(page.entries[0].message(), Some(("user", "hi")));
        assert_eq!(page.next_after, 1);
        assert!(!page.has_more);
    }

    #[test]
    fn non_message_entries_have_no_message() {
        let e: Entry =
            serde_json::from_str(r#"{"seq":2,"type":"fact","payload":{"k":1}}"#).unwrap();
        assert_eq!(e.message(), None);
    }

    #[test]
    fn decodes_session_open_variants() {
        let created: SessionOpen =
            serde_json::from_str(r#"{"sessionId":"srv-1","workspace":"/w"}"#).unwrap();
        assert_eq!(created.session_id, "srv-1");
        assert!(!created.resumed);
        let live: SessionOpen = serde_json::from_str(
            r#"{"sessionId":"srv-1","resumed":false,"alreadyLive":true,"busy":true}"#,
        )
        .unwrap();
        assert!(live.already_live);
        assert_eq!(live.busy, Some(true));
    }

    #[test]
    fn decision_wire_values() {
        assert_eq!(Decision::Allow.as_str(), "allow");
        assert_eq!(Decision::Deny.as_str(), "deny");
    }
}
