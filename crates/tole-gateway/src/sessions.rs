//! Chat-to-session index and the rollover rule (task W1-D).
//!
//! Contract:
//!
//! - One chat = one tole session. The gateway keeps its own index
//!   `chat -> {session id, last owner message time, created time}` in a JSON
//!   file under the state directory, written atomically (temp file plus
//!   rename), tolerant of a missing or empty file, rejecting a corrupt one with
//!   a clear error instead of silently resetting.
//! - Rollover: a new session starts after N hours (default 4) without a
//!   message from the owner, or on `/new`. Only owner messages count; bot
//!   output and cron results never reset the idle clock. A running turn or a
//!   pending approval blocks the automatic rollover (not `/new`, which the app
//!   answers with a refusal while busy). Pure function of
//!   `(state, now, rollover, busy, pending_approval, forced)`.
//! - Time comes from the [`Clock`] trait so tests use a fake clock.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::channel::ChatId;

/// A source of the current time in milliseconds since the Unix epoch.
pub trait Clock: Send + Sync {
    /// Now, in milliseconds since the Unix epoch.
    fn now_ms(&self) -> u64;
}

/// The wall clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }
}

/// The session a chat is bound to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatSession {
    /// The tole serve session id.
    pub session_id: String,
    /// When this session was recorded, ms since the Unix epoch.
    pub created_ms: u64,
    /// Time of the last message from the owner, ms since the Unix epoch.
    pub last_owner_ms: u64,
}

/// Why a new session is planned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewReason {
    /// The chat has no session yet.
    NoSession,
    /// The owner was idle for the rollover period.
    Idle,
    /// The owner asked for it (`/new`).
    Forced,
}

/// What to do with the chat's session before handling a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plan {
    /// Keep using the current session.
    Reuse,
    /// Open a fresh session.
    NewSession {
        /// Why.
        reason: NewReason,
    },
}

/// Decide whether to reuse or replace the session. Pure.
///
/// Idle rollover needs `now - last_owner >= rollover_ms`, no running turn and
/// no pending approval. A forced new session always plans `NewSession`; the
/// app must check [`can_force_new`] first and refuse `/new` while busy.
pub fn plan(
    session: Option<&ChatSession>,
    now_ms: u64,
    rollover_ms: u64,
    busy: bool,
    pending_approval: bool,
    forced_new: bool,
) -> Plan {
    if forced_new {
        return Plan::NewSession {
            reason: NewReason::Forced,
        };
    }
    let Some(s) = session else {
        return Plan::NewSession {
            reason: NewReason::NoSession,
        };
    };
    let idle = now_ms.saturating_sub(s.last_owner_ms) >= rollover_ms;
    if idle && !busy && !pending_approval {
        Plan::NewSession {
            reason: NewReason::Idle,
        }
    } else {
        Plan::Reuse
    }
}

/// `/new` is refused while a turn runs.
pub fn can_force_new(busy: bool) -> bool {
    !busy
}

/// The session index could not be loaded or saved.
#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    /// The file exists but is not a valid index.
    #[error("session index {path} is corrupt: {reason}")]
    Corrupt {
        /// The file.
        path: PathBuf,
        /// The parse error.
        reason: String,
    },
    /// Reading or writing failed.
    #[error("session index {path}: {source}")]
    Io {
        /// The file.
        path: PathBuf,
        /// The I/O error.
        source: std::io::Error,
    },
}

#[derive(Default, Serialize, Deserialize)]
struct IndexFile {
    #[serde(default)]
    chats: BTreeMap<String, ChatSession>,
}

/// The persistent chat to session map.
#[derive(Debug)]
pub struct SessionIndex {
    path: PathBuf,
    chats: BTreeMap<i64, ChatSession>,
}

impl SessionIndex {
    /// Open the index. A missing or empty file is an empty index; a corrupt
    /// one is an error that names the path (it is never reset silently).
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, IndexError> {
        let path = path.into();
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(source) => return Err(IndexError::Io { path, source }),
        };
        let mut chats = BTreeMap::new();
        if !text.trim().is_empty() {
            let corrupt = |reason: String| IndexError::Corrupt {
                path: path.clone(),
                reason,
            };
            let file: IndexFile =
                serde_json::from_str(&text).map_err(|e| corrupt(e.to_string()))?;
            for (k, v) in file.chats {
                let id: i64 = k
                    .parse()
                    .map_err(|_| corrupt(format!("bad chat id {k:?}")))?;
                chats.insert(id, v);
            }
        }
        Ok(Self { path, chats })
    }

    /// The session of a chat, if any.
    pub fn get(&self, chat: ChatId) -> Option<&ChatSession> {
        self.chats.get(&chat.0)
    }

    /// Record a new session for the chat (replacing the old one). The owner is
    /// active now, so `last_owner_ms` starts at `now_ms`.
    pub fn record_new_session(
        &mut self,
        chat: ChatId,
        session_id: &str,
        now_ms: u64,
    ) -> Result<(), IndexError> {
        let mut next = self.chats.clone();
        next.insert(
            chat.0,
            ChatSession {
                session_id: session_id.to_owned(),
                created_ms: now_ms,
                last_owner_ms: now_ms,
            },
        );
        self.commit(next)
    }

    /// Record a message from the owner. Only owner messages call this. No-op
    /// for an unknown chat.
    pub fn touch_owner_message(&mut self, chat: ChatId, now_ms: u64) -> Result<(), IndexError> {
        let mut next = self.chats.clone();
        match next.get_mut(&chat.0) {
            Some(s) => s.last_owner_ms = now_ms,
            None => return Ok(()),
        }
        self.commit(next)
    }

    /// Forget a chat (its next message plans a new session).
    pub fn forget(&mut self, chat: ChatId) -> Result<(), IndexError> {
        if !self.chats.contains_key(&chat.0) {
            return Ok(());
        }
        let mut next = self.chats.clone();
        next.remove(&chat.0);
        self.commit(next)
    }

    /// Save first, then adopt: memory never gets ahead of the disk.
    fn commit(&mut self, next: BTreeMap<i64, ChatSession>) -> Result<(), IndexError> {
        write_atomic(&self.path, &next)?;
        self.chats = next;
        Ok(())
    }
}

fn write_atomic(path: &Path, chats: &BTreeMap<i64, ChatSession>) -> Result<(), IndexError> {
    let io = |source| IndexError::Io {
        path: path.to_path_buf(),
        source,
    };
    let file = IndexFile {
        chats: chats
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect(),
    };
    let json = serde_json::to_vec_pretty(&file).map_err(|e| IndexError::Corrupt {
        path: path.to_path_buf(),
        reason: e.to_string(),
    })?;
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::create_dir_all(dir).map_err(io)?;
    }
    let mut tmp_name = path.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(".tmp");
    let tmp = path.with_file_name(tmp_name);
    let result = (|| {
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(&json)?;
        f.sync_all()?;
        drop(f);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
        }
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result.map_err(io)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: u64 = 3_600_000;
    const CHAT: ChatId = ChatId(5);

    fn sess(last_owner: u64) -> ChatSession {
        ChatSession {
            session_id: "srv-1".into(),
            created_ms: 0,
            last_owner_ms: last_owner,
        }
    }

    #[test]
    fn the_system_clock_is_after_2024() {
        assert!(SystemClock.now_ms() > 1_704_067_200_000);
    }

    #[test]
    fn no_session_plans_new() {
        assert_eq!(
            plan(None, 10, HOUR, false, false, false),
            Plan::NewSession {
                reason: NewReason::NoSession
            }
        );
    }

    #[test]
    fn rollover_boundary_is_inclusive() {
        let s = sess(1_000);
        assert_eq!(
            plan(
                Some(&s),
                1_000 + 4 * HOUR - 1,
                4 * HOUR,
                false,
                false,
                false
            ),
            Plan::Reuse
        );
        assert_eq!(
            plan(Some(&s), 1_000 + 4 * HOUR, 4 * HOUR, false, false, false),
            Plan::NewSession {
                reason: NewReason::Idle
            }
        );
    }

    #[test]
    fn busy_blocks_idle_rollover() {
        let s = sess(0);
        assert_eq!(
            plan(Some(&s), 99 * HOUR, 4 * HOUR, true, false, false),
            Plan::Reuse
        );
    }

    #[test]
    fn pending_approval_blocks_idle_rollover() {
        let s = sess(0);
        assert_eq!(
            plan(Some(&s), 99 * HOUR, 4 * HOUR, false, true, false),
            Plan::Reuse
        );
    }

    #[test]
    fn forced_always_plans_new_and_busy_check_refuses() {
        let s = sess(0);
        for (busy, pending) in [(false, false), (true, false), (false, true), (true, true)] {
            assert_eq!(
                plan(Some(&s), 1, 4 * HOUR, busy, pending, true),
                Plan::NewSession {
                    reason: NewReason::Forced
                }
            );
        }
        assert!(can_force_new(false));
        assert!(!can_force_new(true));
    }

    #[test]
    fn a_clock_step_back_does_not_roll_over() {
        let s = sess(10_000);
        assert_eq!(
            plan(Some(&s), 5_000, HOUR, false, false, false),
            Plan::Reuse
        );
    }

    fn index_path(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join("state").join("sessions.json")
    }

    #[test]
    fn missing_and_empty_files_are_an_empty_index() {
        let dir = tempfile::tempdir().unwrap();
        let p = index_path(&dir);
        assert!(SessionIndex::open(&p).unwrap().get(CHAT).is_none());
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, "  \n").unwrap();
        assert!(SessionIndex::open(&p).unwrap().get(CHAT).is_none());
    }

    #[test]
    fn a_corrupt_file_is_an_error_naming_the_path_and_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sessions.json");
        fs::write(&p, "{not json").unwrap();
        let err = SessionIndex::open(&p).unwrap_err();
        assert!(matches!(err, IndexError::Corrupt { .. }));
        assert!(err.to_string().contains("sessions.json"));
        assert_eq!(fs::read_to_string(&p).unwrap(), "{not json");
        fs::write(
            &p,
            r#"{"chats":{"abc":{"session_id":"s","created_ms":1,"last_owner_ms":1}}}"#,
        )
        .unwrap();
        assert!(SessionIndex::open(&p).is_err());
    }

    #[test]
    fn records_persist_and_reload() {
        let dir = tempfile::tempdir().unwrap();
        let p = index_path(&dir);
        let mut idx = SessionIndex::open(&p).unwrap();
        idx.record_new_session(CHAT, "srv-9", 100).unwrap();
        idx.touch_owner_message(CHAT, 250).unwrap();
        let again = SessionIndex::open(&p).unwrap();
        assert_eq!(
            again.get(CHAT),
            Some(&ChatSession {
                session_id: "srv-9".into(),
                created_ms: 100,
                last_owner_ms: 250
            })
        );
    }

    #[test]
    fn touch_on_unknown_chat_is_a_noop_and_forget_removes() {
        let dir = tempfile::tempdir().unwrap();
        let p = index_path(&dir);
        let mut idx = SessionIndex::open(&p).unwrap();
        idx.touch_owner_message(CHAT, 1).unwrap();
        assert!(idx.get(CHAT).is_none());
        idx.record_new_session(CHAT, "s", 1).unwrap();
        idx.forget(CHAT).unwrap();
        assert!(SessionIndex::open(&p).unwrap().get(CHAT).is_none());
    }

    #[test]
    fn atomic_write_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = index_path(&dir);
        let mut idx = SessionIndex::open(&p).unwrap();
        idx.record_new_session(CHAT, "s", 1).unwrap();
        idx.record_new_session(ChatId(6), "t", 2).unwrap();
        let names: Vec<_> = fs::read_dir(p.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["sessions.json".to_string()]);
    }

    #[cfg(unix)]
    #[test]
    fn index_file_mode_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let p = index_path(&dir);
        let mut idx = SessionIndex::open(&p).unwrap();
        idx.record_new_session(CHAT, "s", 1).unwrap();
        idx.touch_owner_message(CHAT, 2).unwrap();
        let mode = fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn a_failed_save_leaves_memory_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        // The parent of the index path is a file, so the save must fail.
        let blocker = dir.path().join("blocker");
        let mut idx = SessionIndex::open(blocker.join("sessions.json")).unwrap();
        fs::write(&blocker, "x").unwrap();
        assert!(idx.record_new_session(CHAT, "s", 1).is_err());
        assert!(idx.get(CHAT).is_none());
    }
}
