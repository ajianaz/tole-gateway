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
