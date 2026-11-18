//! `tole-gateway`: a chat gateway for `tole serve`.
//!
//! A thin client of the tole serve API. It links no `tole-core` and holds no
//! business logic: sessions, tools, approvals and storage stay in tole. The
//! module plan and decisions live in `docs/design.md`.
//!
//! Module map (one owner per file while the modules are written in parallel):
//!
//! | Module | Role |
//! |--------|------|
//! | [`config`] | Load and validate configuration; secrets never printed. |
//! | [`tole_client`] | HTTP client for the tole serve API ([`tole_client::ToleApi`]). |
//! | [`telegram`] | Telegram adapter: Bot API client, long polling, rendering. |
//! | [`channel`] | The chat-facing ports the adapter implements and the app uses. |
//! | [`auth`] | Allowlist and one-time pairing. |
//! | [`sessions`] | Chat-to-session index and the rollover rule. |
//! | [`approvals`] | Approval rows to buttons; answers to decisions. |
//! | [`app`] | The dispatcher that wires everything together. |
//! | [`runtime`] | Startup: load config, build clients, run the app. |

#![forbid(unsafe_code)]

pub mod app;
pub mod approvals;
pub mod auth;
pub mod channel;
pub mod config;
pub mod error;
pub mod runtime;
pub mod secret;
pub mod sessions;
pub mod telegram;
pub mod tole_client;
pub mod types;
