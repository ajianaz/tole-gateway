//! Allowlist and one-time pairing (task W1-D).
//!
//! Contract:
//!
//! - A chat may reach tole only if its user id is in the configured allowlist
//!   or it has just completed pairing. Everything else is dropped without any
//!   reply (the bot does not reveal that it exists).
//! - Pairing: the server console prints a one-time code; an unknown user who
//!   sends that code as a message is added to the in-memory authorized set (and
//!   nothing else). The code is single use, expires after the configured TTL,
//!   is compared in constant time, and is never logged anywhere but the
//!   console line that shows it to the operator. A wrong code gives no reply
//!   and is rate limited per user.
//! - All time comes from an injected [`crate::sessions::Clock`]; no randomness
//!   from the wall clock. Pure and fully unit tested.
//! - The pairing code generator takes its randomness from an injected source
//!   so tests are deterministic; production uses the OS random source (no new
//!   dependency is required: read it from `/dev/urandom` or use `std` hashing
//!   of `RandomState` only if no better source exists, and document the choice).

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::time::Duration;

use crate::channel::UserId;
use crate::sessions::Clock;

/// Length of a pairing code.
pub const CODE_LEN: usize = 8;
/// Wrong attempts allowed before a user is locked out.
pub const MAX_WRONG_ATTEMPTS: u32 = 5;
/// How long a locked-out user is ignored.
pub const LOCKOUT: Duration = Duration::from_secs(10 * 60);

/// Unambiguous alphabet: 32 symbols, no `0 O 1 I`.
const ALPHABET: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

/// Upper bound of tracked failure records (memory bound against floods).
const MAX_TRACKED_USERS: usize = 1024;

/// Pairing could not start.
#[derive(Debug, thiserror::Error)]
pub enum PairingError {
    /// No secure randomness: no code is produced (fail closed).
    #[error("no secure random source available: {0}")]
    Entropy(String),
}

/// A source of random bytes for pairing codes.
pub trait PairingCodeSource: Send {
    /// Fill `buf` with unpredictable bytes, or fail. Must never return
    /// predictable bytes on failure.
    fn fill(&mut self, buf: &mut [u8]) -> Result<(), PairingError>;
}

/// Production source: reads `/dev/urandom` (no extra dependency). Any read
/// failure is an error, never a fallback.
#[derive(Debug, Default, Clone, Copy)]
pub struct OsPairingCodeSource;

impl PairingCodeSource for OsPairingCodeSource {
    fn fill(&mut self, buf: &mut [u8]) -> Result<(), PairingError> {
        let mut f = File::open("/dev/urandom")
            .map_err(|e| PairingError::Entropy(format!("open /dev/urandom: {e}")))?;
        f.read_exact(buf)
            .map_err(|e| PairingError::Entropy(format!("read /dev/urandom: {e}")))
    }
}

/// A one-time pairing code. `Debug` never prints it; call [`reveal`](Self::reveal)
/// to show it on the server console.
#[derive(Clone, PartialEq, Eq)]
pub struct PairingCode(String);

impl PairingCode {
    /// The code text. Only the operator console line may print this.
    pub fn reveal(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for PairingCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PairingCode(***)")
    }
}

/// Result of a pairing attempt. The app sends no reply for anything but
/// `Paired`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairOutcome {
    /// The user is now authorized for the life of the process.
    Paired,
    /// A code is active and this was not it.
    WrongCode,
    /// No code is active (none started, used, or expired).
    NoActiveCode,
    /// Too many wrong attempts; ignored until the lockout ends.
    Locked,
}

#[derive(Default)]
struct Failures {
    count: u32,
    locked_until_ms: u64,
}

struct ActiveCode {
    code: String,
    expires_ms: u64,
}

/// The allowlist plus in-memory pairing.
pub struct Authorizer<C: Clock, S: PairingCodeSource> {
    allowed: HashSet<UserId>,
    paired: HashSet<UserId>,
    ttl_ms: u64,
    clock: C,
    source: S,
    active: Option<ActiveCode>,
    failures: HashMap<UserId, Failures>,
}

impl<C: Clock, S: PairingCodeSource> std::fmt::Debug for Authorizer<C, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Authorizer")
            .field("allowed", &self.allowed.len())
            .field("paired", &self.paired.len())
            .finish_non_exhaustive()
    }
}

impl<C: Clock, S: PairingCodeSource> Authorizer<C, S> {
    /// Build from the configured allowlist and the pairing code lifetime.
    pub fn new(
        allowlist: impl IntoIterator<Item = UserId>,
        pairing_ttl: Duration,
        clock: C,
        source: S,
    ) -> Self {
        Self {
            allowed: allowlist.into_iter().collect(),
            paired: HashSet::new(),
            ttl_ms: u64::try_from(pairing_ttl.as_millis()).unwrap_or(u64::MAX),
            clock,
            source,
            active: None,
            failures: HashMap::new(),
        }
    }

    /// True when the user is allowlisted or paired in this process.
    pub fn is_authorized(&self, user: UserId) -> bool {
        self.allowed.contains(&user) || self.paired.contains(&user)
    }

    /// Start pairing: replaces any active code. The caller shows the code on
    /// the server console only.
    pub fn start_pairing(&mut self) -> Result<PairingCode, PairingError> {
        let mut raw = [0u8; CODE_LEN];
        self.source.fill(&mut raw)?;
        let code: String = raw
            .iter()
            .map(|b| ALPHABET[(*b & 31) as usize] as char)
            .collect();
        self.active = Some(ActiveCode {
            code: code.clone(),
            expires_ms: self.clock.now_ms().saturating_add(self.ttl_ms),
        });
        Ok(PairingCode(code))
    }

    /// Treat `text` from an unauthorized user as a pairing attempt.
    pub fn try_pair(&mut self, user: UserId, text: &str) -> PairOutcome {
        let now = self.clock.now_ms();
        if let Some(f) = self.failures.get_mut(&user) {
            if f.locked_until_ms > now {
                return PairOutcome::Locked;
            }
            if f.locked_until_ms != 0 {
                *f = Failures::default();
            }
        }
        if self.active.as_ref().is_some_and(|a| now >= a.expires_ms) {
            self.active = None;
        }
        let Some(active) = &self.active else {
            return PairOutcome::NoActiveCode;
        };
        let given = text.trim().to_ascii_uppercase();
        if ct_eq(given.as_bytes(), active.code.as_bytes()) {
            self.active = None;
            self.failures.remove(&user);
            self.paired.insert(user);
            return PairOutcome::Paired;
        }
        if self.failures.len() >= MAX_TRACKED_USERS && !self.failures.contains_key(&user) {
            self.failures.retain(|_, f| f.locked_until_ms > now);
        }
        let f = self.failures.entry(user).or_default();
        f.count += 1;
        if f.count >= MAX_WRONG_ATTEMPTS {
            f.locked_until_ms = now.saturating_add(LOCKOUT.as_millis() as u64);
        }
        PairOutcome::WrongCode
    }
}

/// Constant-time equality: the work does not depend on where the inputs
/// differ (only on the longer length).
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff = (a.len() ^ b.len()) as u64;
    for i in 0..a.len().max(b.len()) {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        diff |= u64::from(x ^ y);
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    #[derive(Clone, Default)]
    struct FakeClock(Arc<AtomicU64>);
    impl FakeClock {
        fn advance(&self, ms: u64) {
            self.0.fetch_add(ms, Ordering::SeqCst);
        }
    }
    impl Clock for FakeClock {
        fn now_ms(&self) -> u64 {
            self.0.load(Ordering::SeqCst)
        }
    }

    /// Deterministic source: counts up so each code differs.
    struct Seq(u8);
    impl PairingCodeSource for Seq {
        fn fill(&mut self, buf: &mut [u8]) -> Result<(), PairingError> {
            for b in buf.iter_mut() {
                self.0 = self.0.wrapping_add(7);
                *b = self.0;
            }
            Ok(())
        }
    }

    struct Broken;
    impl PairingCodeSource for Broken {
        fn fill(&mut self, _: &mut [u8]) -> Result<(), PairingError> {
            Err(PairingError::Entropy("boom".into()))
        }
    }

    fn auth(clock: &FakeClock) -> Authorizer<FakeClock, Seq> {
        Authorizer::new([UserId(1)], Duration::from_secs(300), clock.clone(), Seq(0))
    }

    const U: UserId = UserId(42);

    #[test]
    fn allowlist_is_authorized_others_are_not() {
        let a = auth(&FakeClock::default());
        assert!(a.is_authorized(UserId(1)));
        assert!(!a.is_authorized(U));
    }

    #[test]
    fn constant_time_compare_is_correct() {
        assert!(ct_eq(b"ABCD2345", b"ABCD2345"));
        assert!(!ct_eq(b"ABCD2345", b"ABCD2346"));
        assert!(!ct_eq(b"ABCD2345", b"ABCD234"));
        assert!(!ct_eq(b"", b"A"));
        assert!(ct_eq(b"", b""));
    }

    #[test]
    fn code_format_is_eight_unambiguous_chars() {
        let mut a = auth(&FakeClock::default());
        let code = a.start_pairing().unwrap();
        assert_eq!(code.reveal().len(), CODE_LEN);
        assert!(code.reveal().bytes().all(|b| ALPHABET.contains(&b)));
        assert!(!code.reveal().contains(['0', 'O', '1', 'I']));
    }

    #[test]
    fn debug_never_shows_the_code() {
        let mut a = auth(&FakeClock::default());
        let code = a.start_pairing().unwrap();
        let shown = format!("{code:?} {a:?}");
        assert!(shown.contains("***"));
        assert!(!shown.contains(code.reveal()));
    }

    #[test]
    fn pairing_authorizes_and_is_single_use() {
        let clock = FakeClock::default();
        let mut a = auth(&clock);
        let code = a.start_pairing().unwrap();
        assert_eq!(a.try_pair(U, code.reveal()), PairOutcome::Paired);
        assert!(a.is_authorized(U));
        assert_eq!(
            a.try_pair(UserId(43), code.reveal()),
            PairOutcome::NoActiveCode
        );
        assert!(!a.is_authorized(UserId(43)));
    }

    #[test]
    fn code_is_accepted_case_insensitively_with_whitespace() {
        let mut a = auth(&FakeClock::default());
        let code = a.start_pairing().unwrap();
        let typed = format!("  {}\n", code.reveal().to_lowercase());
        assert_eq!(a.try_pair(U, &typed), PairOutcome::Paired);
    }

    #[test]
    fn no_code_means_no_active_code() {
        let mut a = auth(&FakeClock::default());
        assert_eq!(a.try_pair(U, "ANYTHING"), PairOutcome::NoActiveCode);
    }

    #[test]
    fn code_expires_by_ttl() {
        let clock = FakeClock::default();
        let mut a = auth(&clock);
        let code = a.start_pairing().unwrap();
        clock.advance(299_999);
        // still valid just before; check a different user keeps it unused
        assert_eq!(a.try_pair(U, "WRONGONE"), PairOutcome::WrongCode);
        clock.advance(1);
        assert_eq!(a.try_pair(U, code.reveal()), PairOutcome::NoActiveCode);
        assert!(!a.is_authorized(U));
    }

    #[test]
    fn code_valid_just_before_ttl() {
        let clock = FakeClock::default();
        let mut a = auth(&clock);
        let code = a.start_pairing().unwrap();
        clock.advance(299_999);
        assert_eq!(a.try_pair(U, code.reveal()), PairOutcome::Paired);
    }

    #[test]
    fn five_wrong_attempts_lock_out_even_the_right_code() {
        let clock = FakeClock::default();
        let mut a = Authorizer::new([], Duration::from_secs(3600), clock.clone(), Seq(0));
        let code = a.start_pairing().unwrap();
        for _ in 0..5 {
            assert_eq!(a.try_pair(U, "WRONGONE"), PairOutcome::WrongCode);
        }
        assert_eq!(a.try_pair(U, code.reveal()), PairOutcome::Locked);
        clock.advance(LOCKOUT.as_millis() as u64 - 1);
        assert_eq!(a.try_pair(U, code.reveal()), PairOutcome::Locked);
        clock.advance(1);
        assert_eq!(a.try_pair(U, code.reveal()), PairOutcome::Paired);
    }

    #[test]
    fn four_wrong_attempts_do_not_lock() {
        let mut a = auth(&FakeClock::default());
        let code = a.start_pairing().unwrap();
        for _ in 0..4 {
            assert_eq!(a.try_pair(U, "WRONGONE"), PairOutcome::WrongCode);
        }
        assert_eq!(a.try_pair(U, code.reveal()), PairOutcome::Paired);
    }

    #[test]
    fn lockout_is_per_user() {
        let mut a = auth(&FakeClock::default());
        let code = a.start_pairing().unwrap();
        for _ in 0..5 {
            a.try_pair(U, "WRONGONE");
        }
        assert_eq!(a.try_pair(UserId(7), code.reveal()), PairOutcome::Paired);
    }

    #[test]
    fn new_code_replaces_the_old_one() {
        let mut a = auth(&FakeClock::default());
        let first = a.start_pairing().unwrap();
        let second = a.start_pairing().unwrap();
        assert_ne!(first.reveal(), second.reveal());
        assert_eq!(a.try_pair(U, first.reveal()), PairOutcome::WrongCode);
        assert_eq!(a.try_pair(U, second.reveal()), PairOutcome::Paired);
    }

    #[test]
    fn a_failing_source_fails_closed() {
        let mut a = Authorizer::new([], Duration::from_secs(60), FakeClock::default(), Broken);
        assert!(a.start_pairing().is_err());
        assert_eq!(a.try_pair(U, "ABCDEFGH"), PairOutcome::NoActiveCode);
    }

    #[test]
    fn os_source_produces_varying_bytes() {
        let mut s = OsPairingCodeSource;
        let (mut a, mut b) = ([0u8; 16], [0u8; 16]);
        s.fill(&mut a).unwrap();
        s.fill(&mut b).unwrap();
        assert_ne!(a, b);
    }
}
