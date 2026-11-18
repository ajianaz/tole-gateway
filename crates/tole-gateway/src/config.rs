//! Configuration: a TOML file plus environment overrides for secrets.
//!
//! Scaffold: the section structs and their defaults are fixed here so the other
//! modules can be written in parallel; loading, environment overrides,
//! token-file resolution and validation are filled in by the config task
//! (`Config::load`). Secrets are [`Secret`] values and never printed.

use std::path::PathBuf;

use serde::Deserialize;

use crate::secret::Secret;

fn default_tole_url() -> String {
    "http://127.0.0.1:7801".to_string()
}
fn default_request_timeout_secs() -> u64 {
    30
}
fn default_prompt_timeout_secs() -> u64 {
    1800
}
fn default_telegram_api_base() -> String {
    "https://api.telegram.org".to_string()
}
fn default_poll_timeout_secs() -> u64 {
    30
}
fn default_pairing_ttl_secs() -> u64 {
    600
}
fn default_rollover_hours() -> u64 {
    4
}

/// The whole configuration file.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// How to reach `tole serve`.
    #[serde(default)]
    pub tole: ToleSection,
    /// The Telegram bot.
    #[serde(default)]
    pub telegram: TelegramSection,
    /// Who may talk to the bot.
    #[serde(default)]
    pub auth: AuthSection,
    /// Session behaviour.
    #[serde(default)]
    pub sessions: SessionSection,
    /// Where the gateway keeps its own state (the chat-to-session index).
    #[serde(default)]
    pub state_dir: Option<PathBuf>,
}

/// How the gateway authenticates to `tole serve`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToleAuth {
    /// `Authorization: Bearer <token>`.
    Bearer(Secret),
    /// HTTP Basic, for a serve started with `--auth basic`.
    Basic {
        /// The user name.
        user: String,
        /// The password.
        password: Secret,
    },
}

/// The `[tole]` table.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToleSection {
    /// Base URL of `tole serve` (loopback by default).
    #[serde(default = "default_tole_url")]
    pub url: String,
    /// Bearer token. Prefer the environment variable or `token_file`.
    #[serde(default)]
    pub token: Option<Secret>,
    /// Read the bearer token from this file.
    #[serde(default)]
    pub token_file: Option<PathBuf>,
    /// Basic auth user (when serve runs with `--auth basic`).
    #[serde(default)]
    pub basic_user: Option<String>,
    /// Basic auth password. Prefer the environment variable.
    #[serde(default)]
    pub basic_password: Option<Secret>,
    /// The tole profile every chat session opens under.
    #[serde(default)]
    pub profile: Option<String>,
    /// Working directory for new sessions; the serve workspace when unset.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Timeout for ordinary requests.
    #[serde(default = "default_request_timeout_secs")]
    pub request_timeout_secs: u64,
    /// Timeout for `POST /prompt`, which blocks for the whole turn.
    #[serde(default = "default_prompt_timeout_secs")]
    pub prompt_timeout_secs: u64,
}

impl Default for ToleSection {
    fn default() -> Self {
        Self {
            url: default_tole_url(),
            token: None,
            token_file: None,
            basic_user: None,
            basic_password: None,
            profile: None,
            cwd: None,
            request_timeout_secs: default_request_timeout_secs(),
            prompt_timeout_secs: default_prompt_timeout_secs(),
        }
    }
}

/// The `[telegram]` table.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelegramSection {
    /// The bot token from BotFather. Prefer the environment variable or
    /// `bot_token_file`.
    #[serde(default)]
    pub bot_token: Option<Secret>,
    /// Read the bot token from this file.
    #[serde(default)]
    pub bot_token_file: Option<PathBuf>,
    /// Bot API base URL; override only for tests.
    #[serde(default = "default_telegram_api_base")]
    pub api_base: String,
    /// Long-poll timeout in seconds.
    #[serde(default = "default_poll_timeout_secs")]
    pub poll_timeout_secs: u64,
}

impl Default for TelegramSection {
    fn default() -> Self {
        Self {
            bot_token: None,
            bot_token_file: None,
            api_base: default_telegram_api_base(),
            poll_timeout_secs: default_poll_timeout_secs(),
        }
    }
}

/// The `[auth]` table.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthSection {
    /// Telegram user ids that may use the bot.
    #[serde(default)]
    pub allowed_user_ids: Vec<i64>,
    /// How long a pairing code stays valid.
    #[serde(default = "default_pairing_ttl_secs")]
    pub pairing_ttl_secs: u64,
}

impl Default for AuthSection {
    fn default() -> Self {
        Self {
            allowed_user_ids: Vec::new(),
            pairing_ttl_secs: default_pairing_ttl_secs(),
        }
    }
}

/// The `[sessions]` table.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionSection {
    /// A new session starts after this many hours without an owner message.
    #[serde(default = "default_rollover_hours")]
    pub rollover_hours: u64,
}

impl Default for SessionSection {
    fn default() -> Self {
        Self {
            rollover_hours: default_rollover_hours(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_file_gets_the_documented_defaults() {
        let cfg: Config = toml::from_str("").unwrap();
        assert_eq!(cfg.tole.url, "http://127.0.0.1:7801");
        assert_eq!(cfg.tole.prompt_timeout_secs, 1800);
        assert_eq!(cfg.telegram.api_base, "https://api.telegram.org");
        assert_eq!(cfg.sessions.rollover_hours, 4);
        assert!(cfg.auth.allowed_user_ids.is_empty());
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(toml::from_str::<Config>("[tole]\nurll = \"x\"\n").is_err());
    }

    #[test]
    fn debug_never_shows_secrets() {
        let cfg: Config = toml::from_str("[telegram]\nbot_token = \"123:SECRET\"\n").unwrap();
        assert!(!format!("{cfg:?}").contains("SECRET"));
    }
}
