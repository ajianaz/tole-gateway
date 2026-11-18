//! Configuration: a TOML file plus environment overrides for secrets.
//!
//! [`Config::load`] reads and parses the file, [`Config::validate`] checks it
//! (reporting every problem at once), and the secret accessors
//! ([`Config::tole_auth`], [`Config::bot_token`]) resolve a secret from the
//! environment, then a token file, then the file itself. Secrets are
//! [`Secret`] values and never appear in errors or logs.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;

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

/// Environment lookup, injectable for tests.
pub type EnvFn<'a> = &'a dyn Fn(&str) -> Option<String>;

/// Name of the env var holding the `tole serve` bearer token. The constant is a
/// variable NAME, not a secret, hence the cora-ignore on its line.
pub const ENV_TOLE_TOKEN: &str = "TOLE_SERVE_TOKEN"; // cora-ignore: sec-hardcoded-secret
/// Env var holding the Basic auth user.
pub const ENV_TOLE_BASIC_USER: &str = "TOLE_SERVE_BASIC_USER";
/// Name of the env var holding the Basic auth password (a name, not a secret).
pub const ENV_TOLE_BASIC_PASSWORD: &str = "TOLE_SERVE_BASIC_PASSWORD"; // cora-ignore: sec-hardcoded-secret
/// Name of the env var holding the Telegram bot token (a name, not a secret).
pub const ENV_BOT_TOKEN: &str = "TOLE_GATEWAY_BOT_TOKEN"; // cora-ignore: sec-hardcoded-secret

/// Errors from loading, resolving or validating the configuration. No
/// message contains a secret value or the text of the file.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// The configuration file does not exist.
    #[error("configuration file not found: {}", .0.display())]
    NotFound(PathBuf),
    /// A file could not be read.
    #[error("cannot read {}: {reason}", path.display())]
    Read {
        /// The file.
        path: PathBuf,
        /// The I/O error kind.
        reason: String,
    },
    /// The TOML did not parse or did not match the schema.
    #[error("invalid TOML in {}{location}: {message}", path.display())]
    Parse {
        /// The file.
        path: PathBuf,
        /// `" at line L, column C"`, or empty when unknown.
        location: String,
        /// A message with any offending value removed.
        message: String,
    },
    /// A secret file, or a config file holding inline secrets, is readable
    /// by group or others.
    #[error(
        "file {} has mode {mode:03o}; it must not be readable by group or others (run: chmod 600 {})",
        path.display(), path.display()
    )]
    InsecurePermissions {
        /// The file.
        path: PathBuf,
        /// The permission bits.
        mode: u32,
    },
    /// A secret resolved to an empty value.
    #[error("{what} is blank")]
    Blank {
        /// Which value, naming its source.
        what: String,
    },
    /// No credential was configured.
    #[error("{0}")]
    Missing(String),
    /// More than one scheme or an incomplete pair was configured.
    #[error("{0}")]
    Conflict(String),
    /// Validation failed; one entry per problem.
    #[error("invalid configuration:\n{}", format_problems(.0))]
    Invalid(Vec<String>),
}

fn format_problems(problems: &[String]) -> String {
    problems
        .iter()
        .map(|p| format!("  - {p}"))
        .collect::<Vec<_>>()
        .join("\n")
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

impl Config {
    /// Load the configuration file at `path`, reading the real environment
    /// where needed.
    pub fn load(path: &Path) -> Result<Config, ConfigError> {
        Self::load_with_env(path, &|k| std::env::var(k).ok())
    }

    /// Load with an injected environment lookup. The environment is not
    /// needed to parse the file; it is accepted so callers use one signature.
    pub fn load_with_env(path: &Path, _env: EnvFn<'_>) -> Result<Config, ConfigError> {
        let (text, mode) = read_config_file(path)?;
        let cfg = toml::from_str::<Config>(&text).map_err(|e| parse_error(path, &text, &e))?;
        if let Some(mode) = mode {
            if cfg.has_inline_secret() && mode & 0o077 != 0 {
                return Err(ConfigError::InsecurePermissions {
                    path: path.to_path_buf(),
                    mode,
                });
            }
        }
        Ok(cfg)
    }

    /// True when the file itself carries a secret value.
    fn has_inline_secret(&self) -> bool {
        self.tole.token.is_some()
            || self.tole.basic_password.is_some()
            || self.telegram.bot_token.is_some()
    }

    /// Resolve how to authenticate to `tole serve`. Exactly one scheme
    /// (bearer or Basic) must be configured.
    pub fn tole_auth(&self, env: EnvFn<'_>) -> Result<ToleAuth, ConfigError> {
        let t = &self.tole;
        let bearer = if let Some(v) = env_value(env, ENV_TOLE_TOKEN) {
            Some(nonblank(Secret::new(v), &format!("env {ENV_TOLE_TOKEN}"))?)
        } else if let Some(p) = &t.token_file {
            Some(read_secret_file(p, "tole.token_file")?)
        } else if let Some(s) = &t.token {
            Some(nonblank(s.clone(), "tole.token")?)
        } else {
            None
        };

        let user = env_value(env, ENV_TOLE_BASIC_USER)
            .or_else(|| t.basic_user.clone())
            .map(|u| u.trim().to_string());
        let password = env_value(env, ENV_TOLE_BASIC_PASSWORD)
            .map(Secret::new)
            .or_else(|| t.basic_password.clone());
        let basic = match (user, password) {
            (None, None) => None,
            (Some(u), Some(p)) => {
                if u.trim().is_empty() {
                    return Err(ConfigError::Blank {
                        what: "the Basic auth user".into(),
                    });
                }
                Some((u, nonblank(p, "the Basic auth password")?))
            }
            _ => {
                return Err(ConfigError::Conflict(format!(
                    "Basic auth needs both a user and a password \
                     ({ENV_TOLE_BASIC_USER} and {ENV_TOLE_BASIC_PASSWORD}, \
                     or tole.basic_user and tole.basic_password)"
                )))
            }
        };

        match (bearer, basic) {
            (Some(token), None) => Ok(ToleAuth::Bearer(token)),
            (None, Some((user, password))) => Ok(ToleAuth::Basic { user, password }),
            (Some(_), Some(_)) => Err(ConfigError::Conflict(
                "both a bearer token and Basic credentials are configured for tole serve; \
                 configure exactly one scheme"
                    .into(),
            )),
            (None, None) => Err(ConfigError::Missing(format!(
                "no credentials for tole serve: set {ENV_TOLE_TOKEN}, tole.token_file or \
                 tole.token (bearer), or {ENV_TOLE_BASIC_USER} and {ENV_TOLE_BASIC_PASSWORD} (Basic)"
            ))),
        }
    }

    /// Resolve the Telegram bot token: environment, then
    /// `telegram.bot_token_file`, then `telegram.bot_token`.
    pub fn bot_token(&self, env: EnvFn<'_>) -> Result<Secret, ConfigError> {
        let t = &self.telegram;
        if let Some(v) = env_value(env, ENV_BOT_TOKEN) {
            return nonblank(Secret::new(v), &format!("env {ENV_BOT_TOKEN}"));
        }
        if let Some(p) = &t.bot_token_file {
            return read_secret_file(p, "telegram.bot_token_file");
        }
        if let Some(s) = &t.bot_token {
            return nonblank(s.clone(), "telegram.bot_token");
        }
        Err(ConfigError::Missing(format!(
            "no Telegram bot token: set {ENV_BOT_TOKEN}, telegram.bot_token_file or telegram.bot_token"
        )))
    }

    /// Check every value and report all problems at once. An empty allowlist
    /// is valid (pairing mode).
    pub fn validate(&self) -> Result<(), ConfigError> {
        let mut p: Vec<String> = Vec::new();
        if let Some(m) = url_problem("tole.url", &self.tole.url) {
            p.push(m);
        }
        if self.tole.request_timeout_secs == 0 {
            p.push("tole.request_timeout_secs must be greater than 0".into());
        }
        if self.tole.prompt_timeout_secs == 0 {
            p.push("tole.prompt_timeout_secs must be greater than 0".into());
        }
        if let Some(name) = &self.tole.profile {
            if !valid_profile_name(name) {
                p.push("tole.profile must match [a-z0-9_-]{1,64}".into());
            }
        }
        if let Some(m) = url_problem("telegram.api_base", &self.telegram.api_base) {
            p.push(m);
        }
        if !(1..=50).contains(&self.telegram.poll_timeout_secs) {
            p.push("telegram.poll_timeout_secs must be between 1 and 50".into());
        }
        if self.auth.pairing_ttl_secs < 30 {
            p.push("auth.pairing_ttl_secs must be at least 30".into());
        }
        if self.auth.allowed_user_ids.iter().any(|id| *id <= 0) {
            p.push("auth.allowed_user_ids must contain only positive Telegram user ids".into());
        }
        if self.sessions.rollover_hours < 1 {
            p.push("sessions.rollover_hours must be at least 1".into());
        }
        if p.is_empty() {
            Ok(())
        } else {
            Err(ConfigError::Invalid(p))
        }
    }

    /// Directory for the gateway's own state: `state_dir`, else
    /// `$XDG_STATE_HOME/tole-gateway`, else `$HOME/.local/state/tole-gateway`.
    pub fn state_dir(&self, env: EnvFn<'_>) -> Result<PathBuf, ConfigError> {
        if let Some(d) = &self.state_dir {
            return Ok(d.clone());
        }
        if let Some(x) = env_value(env, "XDG_STATE_HOME").filter(|v| !v.trim().is_empty()) {
            return Ok(PathBuf::from(x).join("tole-gateway"));
        }
        if let Some(h) = env_value(env, "HOME").filter(|v| !v.trim().is_empty()) {
            return Ok(PathBuf::from(h).join(".local/state/tole-gateway"));
        }
        Err(ConfigError::Missing(
            "cannot choose a state directory: set state_dir in the config file, \
             XDG_STATE_HOME or HOME"
                .into(),
        ))
    }
}

/// An env value that is set and non-empty after trimming is `Some`; a set but
/// blank value is reported as `Some("")` so callers can reject it.
fn env_value(env: EnvFn<'_>, key: &str) -> Option<String> {
    env(key)
}

fn nonblank(s: Secret, what: &str) -> Result<Secret, ConfigError> {
    let trimmed = s.expose().trim();
    if trimmed.is_empty() {
        Err(ConfigError::Blank {
            what: what.to_string(),
        })
    } else if trimmed.len() == s.expose().len() {
        Ok(s)
    } else {
        Ok(Secret::new(trimmed))
    }
}

/// Maximum size of a secret file.
const MAX_SECRET_FILE_BYTES: u64 = 4096;

/// Read the config file through one open handle. Returns the text and, on
/// unix, the permission bits of that same handle.
fn read_config_file(path: &Path) -> Result<(String, Option<u32>), ConfigError> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|e| file_error(path, &e))?;
    let meta = file.metadata().map_err(|e| file_error(path, &e))?;
    if !meta.is_file() {
        return Err(read_reason(path, "not a regular file"));
    }
    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        Some(meta.permissions().mode() & 0o777)
    };
    #[cfg(not(unix))]
    let mode = None;
    let mut text = String::new();
    file.take(1024 * 1024)
        .read_to_string(&mut text)
        .map_err(|e| file_error(path, &e))?;
    Ok((text, mode))
}

fn read_reason(path: &Path, reason: &str) -> ConfigError {
    ConfigError::Read {
        path: path.to_path_buf(),
        reason: reason.to_string(),
    }
}

/// Uid of the running process, read from the owner of `/proc/self` (no
/// `unsafe`, no dependency). `None` when it cannot be determined, in which
/// case the ownership check is skipped.
#[cfg(unix)]
fn current_uid() -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/proc/self").ok().map(|m| m.uid())
}

fn read_secret_file(path: &Path, what: &str) -> Result<Secret, ConfigError> {
    use std::io::Read;
    // Open once; every check below describes the very file that is read.
    let file = std::fs::File::open(path).map_err(|e| file_error(path, &e))?;
    let meta = file.metadata().map_err(|e| file_error(path, &e))?;
    if !meta.is_file() {
        return Err(read_reason(path, "not a regular file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(ConfigError::InsecurePermissions {
                path: path.to_path_buf(),
                mode,
            });
        }
        // If the process uid cannot be determined, the owner check is skipped.
        if let Some(uid) = current_uid() {
            if meta.uid() != uid {
                return Err(read_reason(path, "owned by another user"));
            }
        }
    }
    let mut buf = Vec::new();
    file.take(MAX_SECRET_FILE_BYTES + 1)
        .read_to_end(&mut buf)
        .map_err(|e| file_error(path, &e))?;
    if buf.len() as u64 > MAX_SECRET_FILE_BYTES {
        return Err(read_reason(path, "file is larger than 4 KiB"));
    }
    let raw = String::from_utf8(buf).map_err(|_| read_reason(path, "not valid UTF-8"))?;
    nonblank(Secret::new(raw), &format!("{what} ({})", path.display()))
}

fn file_error(path: &Path, e: &std::io::Error) -> ConfigError {
    if e.kind() == std::io::ErrorKind::NotFound {
        ConfigError::NotFound(path.to_path_buf())
    } else {
        ConfigError::Read {
            path: path.to_path_buf(),
            reason: e.kind().to_string(),
        }
    }
}

fn parse_error(path: &Path, text: &str, e: &toml::de::Error) -> ConfigError {
    let location = e
        .span()
        .map(|s| {
            let before = &text[..s.start.min(text.len())];
            let line = before.matches('\n').count() + 1;
            let col = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
            format!(" at line {line}, column {col}")
        })
        .unwrap_or_default();
    // Messages like "invalid type: string \"abc\", expected u64" quote the
    // value; keep only the part after the quote so a secret cannot leak.
    let raw = e.message();
    let message = if raw.starts_with("invalid type") || raw.starts_with("invalid value") {
        match raw.split_once(", expected ") {
            Some((_, rest)) => format!("wrong type or value, expected {rest}"),
            None => "wrong type or value".to_string(),
        }
    } else {
        raw.to_string()
    };
    ConfigError::Parse {
        path: path.to_path_buf(),
        location,
        message,
    }
}

/// Why `u` is not an acceptable service URL, or `None` when it is. The
/// message never echoes the URL, which may carry credentials.
fn url_problem(key: &str, u: &str) -> Option<String> {
    use reqwest::Url;
    let bad = || {
        Some(format!(
            "{key} must be an http:// or https:// URL with a host"
        ))
    };
    if u.chars().any(char::is_whitespace) {
        return bad();
    }
    let Ok(url) = Url::parse(u) else {
        return bad();
    };
    if !matches!(url.scheme(), "http" | "https") {
        return bad();
    }
    let Some(host) = url.host_str().filter(|h| !h.is_empty()) else {
        return bad();
    };
    if !url.username().is_empty() || url.password().is_some() {
        return Some(format!(
            "{key} must not contain a user name or password; \
             use the token keys (token, token_file, basic_user, basic_password, \
             bot_token, bot_token_file) or the environment variables instead"
        ));
    }
    if url.scheme() == "http" && !is_loopback(host) {
        return Some(format!(
            "{key} uses plain http:// to a non-loopback host, which would send \
             credentials in clear text; use https:// or keep the service on loopback"
        ));
    }
    None
}

fn is_loopback(host: &str) -> bool {
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if bare.eq_ignore_ascii_case("localhost") {
        return true;
    }
    bare.parse::<std::net::IpAddr>()
        .is_ok_and(|ip| ip.is_loopback())
}

fn valid_profile_name(n: &str) -> bool {
    (1..=64).contains(&n.len())
        && n.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}
