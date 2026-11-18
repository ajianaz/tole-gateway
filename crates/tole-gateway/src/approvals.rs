//! Approval rows to buttons, and button presses to decisions (task W1-D).
//!
//! Contract:
//!
//! - `prompt_for(row, now) -> ApprovalPrompt`: a plain text message that shows
//!   the tool, a note that it is a Write-tier action, the one-line description
//!   from the row (already redacted by tole; truncate to a sane length, never
//!   add anything that is not in the row), and the time left before it expires
//!   (a denial). Two buttons: Allow and Deny.
//! - Button data is at most 64 bytes (Telegram's limit). Approval ids may be
//!   long: map the id to a short token kept in a bounded in-memory table, or
//!   encode it compactly; reject anything that does not resolve. A press that
//!   does not resolve to a still-pending approval of the same chat is ignored
//!   with a short notice.
//! - `Decision` mapping: Allow means exactly that one call; the gateway never
//!   caches, repeats or auto-approves, and has no code path that approves
//!   without a press by an authorized user in the chat that holds the prompt.
//! - Unanswered means denied: the app denies through the same single gate
//!   (`POST /approvals/{id}/decision`) when the window closes or the user
//!   presses Deny; tole also expires entries on its own after 900 s.
//! - Pure and unit tested; no I/O.

use std::collections::VecDeque;

use crate::channel::{ApprovalPrompt, Button, ChatId};
use crate::types::{ApprovalRow, Decision};

/// Live prompts kept; the oldest is dropped beyond this.
pub const MAX_LIVE: usize = 64;
/// tole expires an approval after this long; unanswered counts as denied.
pub const WINDOW_SECS: u64 = 900;
/// Longest description shown, in characters.
pub const MAX_DESCRIPTION_CHARS: usize = 300;
const MAX_TOOL_CHARS: usize = 100;

/// What a button press means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolve {
    /// A live, same-chat, first press: apply this decision.
    Decide {
        /// The approval to decide.
        approval_id: String,
        /// Allow or Deny, exactly as pressed.
        decision: Decision,
    },
    /// The window closed before the press (counts as denied).
    Expired,
    /// Not a live button: malformed, already used, or dropped.
    Unknown,
    /// The press came from a different chat than the prompt.
    WrongChat,
}

#[derive(Debug)]
struct Entry {
    token: u64,
    chat: ChatId,
    approval_id: String,
    expires_ms: u64,
}

/// Bounded table that maps short button tokens to approvals.
///
/// Every table carries a nonce taken from the start time and put into the
/// button data, so a button from an earlier process (the counter restarts at 1
/// after a restart) can never resolve to a different, newer approval.
#[derive(Debug)]
pub struct ApprovalButtons {
    entries: VecDeque<Entry>,
    next_token: u64,
    nonce: u64,
}

impl Default for ApprovalButtons {
    fn default() -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        Self {
            entries: VecDeque::new(),
            next_token: 0,
            nonce,
        }
    }
}

impl ApprovalButtons {
    /// A table with a fixed nonce, so tests can simulate two processes.
    #[cfg(test)]
    fn with_nonce(nonce: u64) -> Self {
        Self {
            nonce,
            ..Self::default()
        }
    }

    /// An empty table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Build the prompt for a pending row and remember its buttons. A second
    /// prompt for the same approval replaces the first.
    pub fn prompt_for(&mut self, chat: ChatId, row: &ApprovalRow, now_ms: u64) -> ApprovalPrompt {
        let window = if (1..=WINDOW_SECS).contains(&row.expires_in_seconds) {
            row.expires_in_seconds
        } else {
            WINDOW_SECS
        };
        let start = if row.created_at_ms > 0 && row.created_at_ms <= now_ms {
            row.created_at_ms
        } else {
            now_ms
        };
        let expires_ms = start.saturating_add(window * 1000);

        self.entries.retain(|e| e.approval_id != row.id);
        while self.entries.len() >= MAX_LIVE {
            self.entries.pop_front();
        }
        self.next_token += 1;
        let token = self.next_token;
        self.entries.push_back(Entry {
            token,
            chat,
            approval_id: row.id.clone(),
            expires_ms,
        });

        let left_ms = expires_ms.saturating_sub(now_ms);
        let left = if left_ms == 0 {
            "less than a minute".to_owned()
        } else {
            let m = left_ms.div_ceil(60_000);
            format!("{m} minute{}", if m == 1 { "" } else { "s" })
        };
        let mut text = format!(
            "Approval needed: {}\nThis is a Write-tier action and needs the owner's permission.\n",
            one_line(&row.tool, MAX_TOOL_CHARS)
        );
        let desc = one_line(&row.description, MAX_DESCRIPTION_CHARS);
        if !desc.is_empty() {
            text.push_str(&desc);
            text.push('\n');
        }
        text.push_str(&format!("Counts as denied in {left} if unanswered."));
        ApprovalPrompt {
            text,
            buttons: vec![vec![
                Button {
                    label: "Allow".into(),
                    data: format!("a{:x}-{token}:y", self.nonce),
                },
                Button {
                    label: "Deny".into(),
                    data: format!("a{:x}-{token}:n", self.nonce),
                },
            ]],
        }
    }

    /// Resolve a button press. A live press from the right chat consumes the
    /// entry, so a repeat is `Unknown`. Nothing else yields `Allow`.
    pub fn resolve(&mut self, chat: ChatId, data: &str, now_ms: u64) -> Resolve {
        let Some((nonce, token, decision)) = parse(data) else {
            return Resolve::Unknown;
        };
        if nonce != self.nonce {
            // A button from an earlier process: never map it to a new approval.
            return Resolve::Unknown;
        }
        let Some(pos) = self.entries.iter().position(|e| e.token == token) else {
            return Resolve::Unknown;
        };
        if self.entries[pos].chat != chat {
            return Resolve::WrongChat;
        }
        let Some(entry) = self.entries.remove(pos) else {
            return Resolve::Unknown;
        };
        if now_ms >= entry.expires_ms {
            return Resolve::Expired;
        }
        Resolve::Decide {
            approval_id: entry.approval_id,
            decision,
        }
    }

    /// Remove and return the approvals whose window closed unanswered, so the
    /// app can deny them.
    pub fn expired_ids(&mut self, now_ms: u64) -> Vec<String> {
        let mut out = Vec::new();
        self.entries.retain(|e| {
            if now_ms >= e.expires_ms {
                out.push(e.approval_id.clone());
                false
            } else {
                true
            }
        });
        out
    }

    /// Live entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when no prompt is live.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// `a<hex nonce>-<digits>:y` or `...:n`, nothing else.
fn parse(data: &str) -> Option<(u64, u64, Decision)> {
    let rest = data.strip_prefix('a')?;
    let (body, tail) = rest.split_once(':')?;
    let (nonce, num) = body.split_once('-')?;
    if nonce.is_empty()
        || nonce.len() > 16
        || !nonce.bytes().all(|b| b.is_ascii_hexdigit())
        || num.is_empty()
        || !num.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let decision = match tail {
        "y" => Decision::Allow,
        "n" => Decision::Deny,
        _ => return None,
    };
    Some((
        u64::from_str_radix(nonce, 16).ok()?,
        num.parse().ok()?,
        decision,
    ))
}

/// Collapse whitespace, drop control characters, truncate to `max` characters
/// (counted as chars, never splitting one) with an ellipsis when cut.
fn one_line(s: &str, max: usize) -> String {
    let cleaned: String = s
        .chars()
        .filter(|c| !c.is_control() || c.is_whitespace())
        .collect();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max {
        return collapsed;
    }
    let mut out: String = collapsed.chars().take(max - 1).collect();
    out.push('\u{2026}');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ApprovalStatus;

    const CHAT: ChatId = ChatId(7);
    const T0: u64 = 1_000_000;

    fn row(id: &str, desc: &str) -> ApprovalRow {
        ApprovalRow {
            id: id.into(),
            session_id: "srv".into(),
            tool: "write_file".into(),
            description: desc.into(),
            status: ApprovalStatus::Pending,
            created_at_ms: T0,
            expires_in_seconds: 900,
        }
    }

    fn datas(p: &ApprovalPrompt) -> (String, String) {
        (p.buttons[0][0].data.clone(), p.buttons[0][1].data.clone())
    }

    #[test]
    fn prompt_shows_tool_tier_description_and_time_left() {
        let mut t = ApprovalButtons::new();
        let p = t.prompt_for(CHAT, &row("x", "write notes.md"), T0);
        assert!(p.text.contains("write_file"));
        assert!(p.text.contains("Write-tier action"));
        assert!(p.text.contains("owner's permission"));
        assert!(p.text.contains("write notes.md"));
        assert!(p.text.contains("15 minutes"));
        assert_eq!(p.buttons.len(), 1);
        assert_eq!(p.buttons[0][0].label, "Allow");
        assert_eq!(p.buttons[0][1].label, "Deny");
    }

    #[test]
    fn time_left_counts_down_and_rounds_up() {
        let mut t = ApprovalButtons::new();
        let p = t.prompt_for(CHAT, &row("x", "d"), T0 + 840_000 + 1);
        assert!(p.text.contains("1 minute "), "{}", p.text);
    }

    #[test]
    fn button_data_is_short_and_has_no_approval_id() {
        let mut t = ApprovalButtons::new();
        let long_id = "apr-".to_string() + &"z".repeat(200);
        let p = t.prompt_for(CHAT, &row(&long_id, "d"), T0);
        let (y, n) = datas(&p);
        assert!(y.len() <= 64 && n.len() <= 64);
        assert!(!y.contains("apr-"));
        assert!(y.ends_with(":y") && n.ends_with(":n"));
    }

    #[test]
    fn allow_and_deny_resolve_to_their_decisions() {
        let mut t = ApprovalButtons::new();
        let p = t.prompt_for(CHAT, &row("a1", "d"), T0);
        let (y, _) = datas(&p);
        assert_eq!(
            t.resolve(CHAT, &y, T0 + 1),
            Resolve::Decide {
                approval_id: "a1".into(),
                decision: Decision::Allow
            }
        );
        let p = t.prompt_for(CHAT, &row("a2", "d"), T0);
        let (_, n) = datas(&p);
        assert_eq!(
            t.resolve(CHAT, &n, T0 + 1),
            Resolve::Decide {
                approval_id: "a2".into(),
                decision: Decision::Deny
            }
        );
    }

    #[test]
    fn a_second_press_is_unknown_even_of_the_other_button() {
        let mut t = ApprovalButtons::new();
        let p = t.prompt_for(CHAT, &row("a1", "d"), T0);
        let (y, n) = datas(&p);
        assert!(matches!(t.resolve(CHAT, &y, T0), Resolve::Decide { .. }));
        assert_eq!(t.resolve(CHAT, &y, T0), Resolve::Unknown);
        assert_eq!(t.resolve(CHAT, &n, T0), Resolve::Unknown);
    }

    #[test]
    fn wrong_chat_is_refused_and_does_not_consume() {
        let mut t = ApprovalButtons::new();
        let p = t.prompt_for(CHAT, &row("a1", "d"), T0);
        let (y, _) = datas(&p);
        assert_eq!(t.resolve(ChatId(8), &y, T0), Resolve::WrongChat);
        assert!(matches!(t.resolve(CHAT, &y, T0), Resolve::Decide { .. }));
    }

    #[test]
    fn expiry_boundary_899s_ok_900s_expired() {
        let mut t = ApprovalButtons::new();
        let p = t.prompt_for(CHAT, &row("a1", "d"), T0);
        let (y, _) = datas(&p);
        assert!(matches!(
            t.resolve(CHAT, &y, T0 + 899_000),
            Resolve::Decide { .. }
        ));
        let p = t.prompt_for(CHAT, &row("a2", "d"), T0);
        let (y, _) = datas(&p);
        assert_eq!(t.resolve(CHAT, &y, T0 + 900_000), Resolve::Expired);
        assert_eq!(t.resolve(CHAT, &y, T0 + 900_000), Resolve::Unknown);
    }

    #[test]
    fn expiry_counts_from_the_row_creation() {
        let mut t = ApprovalButtons::new();
        let p = t.prompt_for(CHAT, &row("a1", "d"), T0 + 600_000);
        assert!(p.text.contains("5 minutes"));
        let (y, _) = datas(&p);
        assert_eq!(t.resolve(CHAT, &y, T0 + 900_000), Resolve::Expired);
    }

    #[test]
    fn a_future_created_time_cannot_extend_the_window() {
        let mut t = ApprovalButtons::new();
        let mut r = row("a1", "d");
        r.created_at_ms = T0 + 10_000_000;
        let p = t.prompt_for(CHAT, &r, T0);
        let (y, _) = datas(&p);
        assert_eq!(t.resolve(CHAT, &y, T0 + 900_000), Resolve::Expired);
    }

    #[test]
    fn expired_ids_returns_and_removes_unanswered() {
        let mut t = ApprovalButtons::new();
        t.prompt_for(CHAT, &row("old", "d"), T0);
        let mut later = row("new", "d");
        later.created_at_ms = T0 + 500_000;
        t.prompt_for(CHAT, &later, T0 + 500_000);
        assert!(t.expired_ids(T0 + 899_000).is_empty());
        assert_eq!(t.expired_ids(T0 + 900_000), vec!["old".to_string()]);
        assert_eq!(t.len(), 1);
        assert_eq!(t.expired_ids(T0 + 900_000), Vec::<String>::new());
    }

    #[test]
    fn garbage_data_is_unknown() {
        let mut t = ApprovalButtons::new();
        t.prompt_for(CHAT, &row("a1", "d"), T0);
        for bad in [
            "",
            "a",
            "a1",
            "a1:",
            "a1:x",
            "a1:y ",
            "b1:y",
            "a+1:y",
            "a-1:y",
            "a1:Y",
            "a99999999999999999999999:y",
            "a1:y:y",
        ] {
            assert_eq!(t.resolve(CHAT, bad, T0), Resolve::Unknown, "{bad:?}");
        }
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn the_table_is_bounded_and_drops_the_oldest() {
        let mut t = ApprovalButtons::new();
        let mut first = None;
        for i in 0..(MAX_LIVE + 5) {
            let p = t.prompt_for(CHAT, &row(&format!("a{i}"), "d"), T0);
            if i == 0 {
                first = Some(datas(&p).0);
            }
            assert!(t.len() <= MAX_LIVE);
        }
        assert_eq!(t.len(), MAX_LIVE);
        assert_eq!(t.resolve(CHAT, &first.unwrap(), T0), Resolve::Unknown);
    }

    #[test]
    fn newest_entries_survive_the_bound() {
        let mut t = ApprovalButtons::new();
        let mut last = None;
        for i in 0..(MAX_LIVE + 5) {
            last = Some(datas(&t.prompt_for(CHAT, &row(&format!("a{i}"), "d"), T0)).0);
        }
        assert!(matches!(
            t.resolve(CHAT, &last.unwrap(), T0),
            Resolve::Decide { .. }
        ));
    }

    #[test]
    fn re_prompting_the_same_approval_invalidates_the_old_buttons() {
        let mut t = ApprovalButtons::new();
        let (old, _) = datas(&t.prompt_for(CHAT, &row("a1", "d"), T0));
        let (new, _) = datas(&t.prompt_for(CHAT, &row("a1", "d"), T0));
        assert_eq!(t.len(), 1);
        assert_eq!(t.resolve(CHAT, &old, T0), Resolve::Unknown);
        assert!(matches!(t.resolve(CHAT, &new, T0), Resolve::Decide { .. }));
    }

    #[test]
    fn long_descriptions_truncate_on_char_boundaries() {
        let desc = "\u{e9}\u{4e16}\u{1f600}".repeat(200); // multi-byte chars
        let mut t = ApprovalButtons::new();
        let p = t.prompt_for(CHAT, &row("a1", &desc), T0);
        let line = p.text.lines().nth(2).unwrap();
        assert_eq!(line.chars().count(), MAX_DESCRIPTION_CHARS);
        assert!(line.ends_with('\u{2026}'));
    }

    #[test]
    fn exactly_300_chars_is_not_truncated() {
        let desc = "\u{4e16}".repeat(300);
        let mut t = ApprovalButtons::new();
        let p = t.prompt_for(CHAT, &row("a1", &desc), T0);
        assert!(p.text.contains(&desc));
        assert!(!p.text.contains('\u{2026}'));
    }

    #[test]
    fn whitespace_collapses_and_control_chars_go() {
        let mut t = ApprovalButtons::new();
        let p = t.prompt_for(CHAT, &row("a1", "write\n\n  a\t\tfile\u{7}  now"), T0);
        assert!(p.text.contains("write a file now"));
        assert_eq!(p.text.lines().count(), 4);
    }

    #[test]
    fn empty_description_adds_nothing() {
        let mut t = ApprovalButtons::new();
        let p = t.prompt_for(CHAT, &row("a1", "  "), T0);
        assert_eq!(p.text.lines().count(), 3);
    }
    #[test]
    fn a_button_from_an_earlier_process_never_resolves_to_a_new_approval() {
        // Both tables hand out counter 1, as a restart would.
        let mut old = ApprovalButtons::with_nonce(0xAAAA);
        let (stale_allow, stale_deny) = datas(&old.prompt_for(CHAT, &row("old", "d"), T0));
        let mut fresh = ApprovalButtons::with_nonce(0xBBBB);
        let (fresh_allow, _) = datas(&fresh.prompt_for(CHAT, &row("new", "d"), T0));
        assert_eq!(fresh.resolve(CHAT, &stale_allow, T0), Resolve::Unknown);
        assert_eq!(fresh.resolve(CHAT, &stale_deny, T0), Resolve::Unknown);
        // The new approval is untouched and still resolves with its own button.
        assert_eq!(
            fresh.resolve(CHAT, &fresh_allow, T0),
            Resolve::Decide {
                approval_id: "new".into(),
                decision: Decision::Allow,
            }
        );
    }

    #[test]
    fn button_data_stays_within_the_telegram_limit() {
        let mut t = ApprovalButtons::with_nonce(u64::MAX);
        t.next_token = 999_999_999;
        let (a, d) = datas(&t.prompt_for(CHAT, &row("x", "d"), T0));
        assert!(a.len() <= 64 && d.len() <= 64, "{a} {d}");
    }
}
