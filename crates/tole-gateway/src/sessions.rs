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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_system_clock_is_after_2024() {
        assert!(SystemClock.now_ms() > 1_704_067_200_000);
    }
}
