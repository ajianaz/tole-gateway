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
