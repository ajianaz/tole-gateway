//! The dispatcher (task W2, after W1-A to W1-D land).
//!
//! It wires [`crate::channel::ChatSource`], [`crate::channel::ChatPort`],
//! [`crate::tole_client::ToleApi`], the allowlist, the session index and the
//! approval buttons together:
//!
//! - an authorized text message becomes `POST /sessions/{id}/prompt`;
//! - `approval_required` becomes an approval prompt; a press decides through
//!   `POST /approvals/{id}/decision`;
//! - after an allow the resumed turn has no HTTP answer: poll
//!   `GET /approvals?session=` and `GET /sessions/{id}/status` until idle, then
//!   read the new entries to deliver the final answer;
//! - one turn at a time per chat; commands `/new`, `/cancel`, `/status`,
//!   `/help`;
//! - errors reach the chat as short, secret-free sentences.
