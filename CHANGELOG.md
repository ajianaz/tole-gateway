# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project uses `0.x` versions until the owner decides otherwise.

## [Unreleased]

### Fixed
- Decisions follow tole's first-decision-only route (tole #938). A `409` answer (the request was already decided the other way, or had expired) is reported as "already approved/denied/expired" instead of "could not record", and no denial is queued for it. An Allow whose answer was lost is retried once, which lets tole finish the audit and start the resume; before, the gateway watched a session that never resumed.

### Added
- Optional receiver for tole's signed outbound webhooks (`[notify]`, off by default, loopback unless told otherwise; see `docs/notify.md`). It verifies the v2 HMAC over the raw body in constant time, a 300 second timestamp window and the headers against the body, dedupes on the delivery id (tole retries at least once), caps the body, answers every refusal with the same bare `401`, and keeps the key as a `Secret` taken from the environment or a 0600 file. An approval raised by a resumed turn now reaches the chat without polling for it; polling stays as a slow fallback and when the receiver is off. tole has no "turn finished" event, so the status and log reads after an Allow remain.
- Telegram gateway: the dispatcher that turns chat messages into tole turns, shows approvals as Allow and Deny buttons, waits for the resumed turn after an allow, denies approvals that nobody answers, and handles `/new`, `/cancel`, `/status` and `/help` with one turn at a time per chat.
- `tole serve` HTTP client with typed errors and the Telegram Bot API adapter (long polling, safe plain-text rendering, message splitting).
- Allowlist and one-time console pairing, the chat-to-session index with the four-hour idle rollover, and bounded one-shot approval buttons.
- Configuration file with environment overrides for secrets, validation that reports every problem at once, and `--config` / `--check` on the command line.
- Repository bootstrap: Cargo workspace with the `tole-gateway` binary skeleton (`--version`, `--help`), CI and governance workflows, commit-msg hook, contributor docs and the design document with the G1 to G5 roadmap.
