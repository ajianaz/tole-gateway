# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project uses `0.x` versions until the owner decides otherwise.

## [Unreleased]

### Added
- Telegram gateway: the dispatcher that turns chat messages into tole turns, shows approvals as Allow and Deny buttons, waits for the resumed turn after an allow, denies approvals that nobody answers, and handles `/new`, `/cancel`, `/status` and `/help` with one turn at a time per chat.
- `tole serve` HTTP client with typed errors and the Telegram Bot API adapter (long polling, safe plain-text rendering, message splitting).
- Allowlist and one-time console pairing, the chat-to-session index with the four-hour idle rollover, and bounded one-shot approval buttons.
- Configuration file with environment overrides for secrets, validation that reports every problem at once, and `--config` / `--check` on the command line.
- Repository bootstrap: Cargo workspace with the `tole-gateway` binary skeleton (`--version`, `--help`), CI and governance workflows, commit-msg hook, contributor docs and the design document with the G1 to G5 roadmap.
