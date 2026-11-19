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
fn default_notify_listen() -> String {
    "127.0.0.1:7811".to_string()
}
fn default_notify_path() -> String {
    "/tole/events".to_string()
}
fn default_notify_destination() -> String {
    "gateway".to_string()
}
fn default_notify_max_body_bytes() -> usize {
    64 * 1024
}
fn default_notify_fallback_poll_secs() -> u64 {
    15
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
/// Name of the env var holding the webhook signing key (a name, not a secret).
pub const ENV_WEBHOOK_KEY: &str = "TOLE_GATEWAY_WEBHOOK_KEY"; // cora-ignore: sec-hardcoded-secret
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
    /// The optional receiver for tole's signed outbound webhooks.
    #[serde(default)]
    pub notify: NotifySection,
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

/// The `[notify]` table: the receiver for tole's `[webhook_out.<name>]`
/// deliveries. Off by default; plain HTTP on loopback by design (put it behind
/// your own reverse proxy or private network, see `docs/notify.md`). The
/// signing key is never a config value: environment or key file only.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotifySection {
    /// Start the receiver. Default false: nothing listens.
    #[serde(default)]
    pub enabled: bool,
    /// `host:port` to bind. Must be a loopback address unless
    /// `allow_non_loopback` is set.
    #[serde(default = "default_notify_listen")]
    pub listen: String,
    /// Allow a non-loopback `listen` address (behind your own proxy or a
    /// private network only). Default false.
    #[serde(default)]
    pub allow_non_loopback: bool,
    /// The URL path tole posts to.
    #[serde(default = "default_notify_path")]
    pub path: String,
    /// The name of the tole `[webhook_out.<name>]` destination; a body that
    /// names another destination is refused.
    #[serde(default = "default_notify_destination")]
    pub destination: String,
    /// Read the HMAC signing key from this file (chmod 600).
    #[serde(default)]
    pub hmac_key_file: Option<PathBuf>,
    /// Largest request body accepted, in bytes (1 KiB to 1 MiB).
    #[serde(default = "default_notify_max_body_bytes")]
    pub max_body_bytes: usize,
    /// While the receiver is on, how often the open approvals are still
    /// polled as a fallback, in seconds (1 to 3600).
    #[serde(default = "default_notify_fallback_poll_secs")]
    pub fallback_poll_secs: u64,
}

impl Default for NotifySection {
    fn default() -> Self {
        Self {
            enabled: false,
            listen: default_notify_listen(),
            allow_non_loopback: false,
            path: default_notify_path(),
            destination: default_notify_destination(),
            hmac_key_file: None,
            max_body_bytes: default_notify_max_body_bytes(),
            fallback_poll_secs: default_notify_fallback_poll_secs(),
        }
    }
}

/// Shortest accepted signing key, in bytes (tole refuses shorter ones too).
pub const MIN_WEBHOOK_KEY_BYTES: usize = 16;

impl Config {
    /// Resolve the webhook signing key: environment, then
    /// `notify.hmac_key_file`. Never an inline config value.
    pub fn webhook_key(&self, env: EnvFn<'_>) -> Result<Secret, ConfigError> {
        let key = if let Some(v) = env_value(env, ENV_WEBHOOK_KEY) {
            nonblank(Secret::new(v), &format!("env {ENV_WEBHOOK_KEY}"))?
        } else if let Some(p) = &self.notify.hmac_key_file {
            read_secret_file(p, "notify.hmac_key_file")?
        } else {
            return Err(ConfigError::Missing(format!(
                "no webhook signing key: set {ENV_WEBHOOK_KEY} or notify.hmac_key_file"
            )));
        };
        if key.expose().len() < MIN_WEBHOOK_KEY_BYTES {
            return Err(ConfigError::Missing(format!(
                "the webhook signing key must be at least {MIN_WEBHOOK_KEY_BYTES} bytes"
            )));
        }
        Ok(key)
    }

    /// The address to bind, when the receiver is enabled and valid.
    pub fn notify_addr(&self) -> Option<std::net::SocketAddr> {
        self.notify.listen.parse().ok()
    }

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
        self.validate_notify(&mut p);
        if p.is_empty() {
            Ok(())
        } else {
            Err(ConfigError::Invalid(p))
        }
    }

    fn validate_notify(&self, p: &mut Vec<String>) {
        let n = &self.notify;
        if !n.enabled {
            return;
        }
        match n.listen.parse::<std::net::SocketAddr>() {
            Err(_) => p.push("notify.listen must be an ip:port address".into()),
            Ok(a) if !a.ip().is_loopback() && !n.allow_non_loopback => p.push(
                "notify.listen is not a loopback address; bind loopback and put your reverse \
                 proxy in front, or set notify.allow_non_loopback = true on a private network"
                    .into(),
            ),
            Ok(_) => {}
        }
        if !n.path.starts_with('/') || n.path.len() > 200 || n.path.contains(['?', '#', ' ']) {
            p.push("notify.path must start with / and hold no query or space".into());
        }
        if !valid_profile_name(&n.destination) || n.destination.len() > 32 {
            p.push("notify.destination must match [a-z0-9_-]{1,32}".into());
        }
        if !(1024..=1024 * 1024).contains(&n.max_body_bytes) {
            p.push("notify.max_body_bytes must be between 1024 and 1048576".into());
        }
        if !(1..=3600).contains(&n.fallback_poll_secs) {
            p.push("notify.fallback_poll_secs must be between 1 and 3600".into());
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

    use std::collections::HashMap;
    use std::os::unix::fs::PermissionsExt;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| m.get(k).cloned()
    }

    fn cfg(text: &str) -> Config {
        toml::from_str(text).unwrap()
    }

    fn token_file(dir: &tempfile::TempDir, name: &str, body: &str, mode: u32) -> PathBuf {
        let p = dir.path().join(name);
        std::fs::write(&p, body).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
        p
    }

    #[test]
    fn load_reads_a_file_and_reports_a_missing_one() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("c.toml");
        std::fs::write(&p, "[sessions]\nrollover_hours = 9\n").unwrap();
        let c = Config::load_with_env(&p, &env_of(&[])).unwrap();
        assert_eq!(c.sessions.rollover_hours, 9);
        let missing = dir.path().join("nope.toml");
        let e = Config::load(&missing).unwrap_err();
        assert!(matches!(e, ConfigError::NotFound(_)));
        assert!(e.to_string().contains("nope.toml"));
    }

    #[test]
    fn toml_errors_have_line_and_column_but_no_values() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("c.toml");
        std::fs::write(
            &p,
            "[telegram]\nbot_token = \"SECRETVALUE\"\npoll_timeout_secs = \"SECRETVALUE\"\n",
        )
        .unwrap();
        let e = Config::load(&p).unwrap_err().to_string();
        assert!(e.contains("line 3"), "{e}");
        assert!(e.contains("column"), "{e}");
        assert!(!e.contains("SECRETVALUE"), "{e}");
    }

    #[test]
    fn bearer_precedence_is_env_then_file_then_inline() {
        let dir = tempfile::tempdir().unwrap();
        let f = token_file(&dir, "t", "from-file\n", 0o600);
        let c = cfg(&format!(
            "[tole]\ntoken = \"inline\"\ntoken_file = \"{}\"\n",
            f.display()
        ));
        let auth = c
            .tole_auth(&env_of(&[(ENV_TOLE_TOKEN, "from-env")]))
            .unwrap();
        assert_eq!(auth, ToleAuth::Bearer(Secret::new("from-env")));
        let auth = c.tole_auth(&env_of(&[])).unwrap();
        assert_eq!(auth, ToleAuth::Bearer(Secret::new("from-file")));
        let c = cfg("[tole]\ntoken = \"inline\"\n");
        assert_eq!(
            c.tole_auth(&env_of(&[])).unwrap(),
            ToleAuth::Bearer(Secret::new("inline"))
        );
    }

    #[test]
    fn basic_auth_from_env_or_file() {
        let c = cfg("[tole]\nbasic_user = \"u\"\nbasic_password = \"p\"\n");
        assert_eq!(
            c.tole_auth(&env_of(&[])).unwrap(),
            ToleAuth::Basic {
                user: "u".into(),
                password: Secret::new("p")
            }
        );
        let e = env_of(&[(ENV_TOLE_BASIC_USER, "eu"), (ENV_TOLE_BASIC_PASSWORD, "ep")]);
        assert_eq!(
            c.tole_auth(&e).unwrap(),
            ToleAuth::Basic {
                user: "eu".into(),
                password: Secret::new("ep")
            }
        );
    }

    #[test]
    fn both_or_neither_scheme_is_an_error() {
        let both = cfg("[tole]\ntoken = \"t\"\nbasic_user = \"u\"\nbasic_password = \"p\"\n");
        assert!(matches!(
            both.tole_auth(&env_of(&[])),
            Err(ConfigError::Conflict(_))
        ));
        let none = cfg("");
        assert!(matches!(
            none.tole_auth(&env_of(&[])),
            Err(ConfigError::Missing(_))
        ));
        let half = cfg("[tole]\nbasic_user = \"u\"\n");
        assert!(matches!(
            half.tole_auth(&env_of(&[])),
            Err(ConfigError::Conflict(_))
        ));
    }

    #[test]
    fn blank_values_are_errors() {
        let c = cfg("[tole]\ntoken = \"t\"\n");
        assert!(matches!(
            c.tole_auth(&env_of(&[(ENV_TOLE_TOKEN, "  ")])),
            Err(ConfigError::Blank { .. })
        ));
        assert!(matches!(
            cfg("[tole]\ntoken = \"\"\n").tole_auth(&env_of(&[])),
            Err(ConfigError::Blank { .. })
        ));
        assert!(matches!(
            cfg("[telegram]\nbot_token = \" \"\n").bot_token(&env_of(&[])),
            Err(ConfigError::Blank { .. })
        ));
        let dir = tempfile::tempdir().unwrap();
        let f = token_file(&dir, "empty", "\n", 0o600);
        let c = cfg(&format!(
            "[telegram]\nbot_token_file = \"{}\"\n",
            f.display()
        ));
        assert!(matches!(
            c.bot_token(&env_of(&[])),
            Err(ConfigError::Blank { .. })
        ));
    }

    #[test]
    fn bot_token_precedence_and_newline_trim() {
        let dir = tempfile::tempdir().unwrap();
        let f = token_file(&dir, "bot", "123:file\r\n", 0o600);
        let c = cfg(&format!(
            "[telegram]\nbot_token = \"123:inline\"\nbot_token_file = \"{}\"\n",
            f.display()
        ));
        assert_eq!(
            c.bot_token(&env_of(&[(ENV_BOT_TOKEN, "123:env")])).unwrap(),
            Secret::new("123:env")
        );
        assert_eq!(c.bot_token(&env_of(&[])).unwrap(), Secret::new("123:file"));
        assert_eq!(
            cfg("[telegram]\nbot_token = \"123:inline\"\n")
                .bot_token(&env_of(&[]))
                .unwrap(),
            Secret::new("123:inline")
        );
        assert!(matches!(
            cfg("").bot_token(&env_of(&[])),
            Err(ConfigError::Missing(_))
        ));
    }

    #[test]
    fn token_files_must_not_be_group_or_world_readable() {
        let dir = tempfile::tempdir().unwrap();
        let f = token_file(&dir, "bot", "SECRETVALUE", 0o640);
        let c = cfg(&format!(
            "[telegram]\nbot_token_file = \"{}\"\n",
            f.display()
        ));
        let e = c.bot_token(&env_of(&[])).unwrap_err();
        assert!(matches!(e, ConfigError::InsecurePermissions { .. }));
        let msg = e.to_string();
        assert!(msg.contains("chmod 600"), "{msg}");
        assert!(!msg.contains("SECRETVALUE"));
    }

    #[test]
    fn a_missing_token_file_names_the_path() {
        let c = cfg("[telegram]\nbot_token_file = \"/nonexistent/dir/tok\"\n");
        let e = c.bot_token(&env_of(&[])).unwrap_err().to_string();
        assert!(e.contains("/nonexistent/dir/tok"), "{e}");
    }

    #[test]
    fn validate_accepts_defaults_and_an_empty_allowlist() {
        cfg("").validate().unwrap();
        cfg("[auth]\nallowed_user_ids = []\n").validate().unwrap();
    }

    #[test]
    fn validate_reports_every_problem() {
        let c = cfg(
            "[tole]\nurl = \"ftp://x\"\nrequest_timeout_secs = 0\nprompt_timeout_secs = 0\n\
             profile = \"Bad Name\"\n[telegram]\napi_base = \"nope\"\npoll_timeout_secs = 99\n\
             [auth]\nallowed_user_ids = [1, -5, 0]\npairing_ttl_secs = 5\n\
             [sessions]\nrollover_hours = 0\n",
        );
        let Err(ConfigError::Invalid(p)) = c.validate() else {
            panic!("expected Invalid");
        };
        assert_eq!(p.len(), 9, "{p:?}");
        let text = ConfigError::Invalid(p).to_string();
        assert!(text.contains("tole.url") && text.contains("rollover_hours"));
    }

    #[test]
    fn profile_name_rule() {
        assert!(valid_profile_name("work_1-a"));
        assert!(!valid_profile_name(""));
        assert!(!valid_profile_name("Upper"));
        assert!(!valid_profile_name(&"a".repeat(65)));
        assert!(valid_profile_name(&"a".repeat(64)));
    }

    #[test]
    fn state_dir_precedence() {
        let c = cfg("state_dir = \"/srv/gw\"\n");
        assert_eq!(
            c.state_dir(&env_of(&[("XDG_STATE_HOME", "/x")])).unwrap(),
            PathBuf::from("/srv/gw")
        );
        let c = cfg("");
        assert_eq!(
            c.state_dir(&env_of(&[("XDG_STATE_HOME", "/x"), ("HOME", "/h")]))
                .unwrap(),
            PathBuf::from("/x/tole-gateway")
        );
        assert_eq!(
            c.state_dir(&env_of(&[("HOME", "/h")])).unwrap(),
            PathBuf::from("/h/.local/state/tole-gateway")
        );
        assert!(c.state_dir(&env_of(&[])).is_err());
    }

    fn problems(text: &str) -> Vec<String> {
        match cfg(text).validate() {
            Err(ConfigError::Invalid(p)) => p,
            Ok(()) => vec![],
            Err(e) => panic!("{e}"),
        }
    }

    #[test]
    fn urls_with_userinfo_or_no_host_are_rejected_without_echo() {
        for u in [
            "http://user:PWSECRET@127.0.0.1:7801",
            "https://tok@example.com",
        ] {
            let p = problems(&format!("[tole]\nurl = \"{u}\"\n"));
            assert_eq!(p.len(), 1, "{p:?}");
            assert!(p[0].contains("token"), "{p:?}");
            assert!(
                !p[0].contains("PWSECRET") && !p[0].contains("tok@"),
                "{p:?}"
            );
            let p = problems(&format!("[telegram]\napi_base = \"{u}\"\n"));
            assert_eq!(p.len(), 1, "{p:?}");
        }
        for u in ["http://:80", "http://@/", "https://"] {
            assert_eq!(
                problems(&format!("[tole]\nurl = \"{u}\"\n")).len(),
                1,
                "{u}"
            );
        }
    }

    #[test]
    fn plain_http_is_only_allowed_on_loopback() {
        for u in [
            "http://localhost:7801",
            "http://127.0.0.1:7801",
            "http://127.5.6.7",
            "http://[::1]:7801",
            "https://10.0.0.5:7801",
            "https://example.com",
        ] {
            assert!(
                problems(&format!("[tole]\nurl = \"{u}\"\n")).is_empty(),
                "{u}"
            );
            assert!(
                problems(&format!("[telegram]\napi_base = \"{u}\"\n")).is_empty(),
                "{u}"
            );
        }
        for u in [
            "http://10.0.0.5:7801",
            "http://example.com",
            "http://[2001:db8::1]",
        ] {
            let p = problems(&format!("[tole]\nurl = \"{u}\"\n"));
            assert_eq!(p.len(), 1, "{u}");
            assert!(p[0].contains("https"), "{p:?}");
            let p = problems(&format!("[telegram]\napi_base = \"{u}\"\n"));
            assert_eq!(p.len(), 1, "{u}");
        }
    }

    #[test]
    fn token_files_must_be_regular_files() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(&format!(
            "[telegram]\nbot_token_file = \"{}\"\n",
            dir.path().display()
        ));
        let e = c.bot_token(&env_of(&[])).unwrap_err();
        assert!(e.to_string().contains("not a regular file"), "{e}");
        let c = cfg("[telegram]\nbot_token_file = \"/dev/null\"\n");
        let e = c.bot_token(&env_of(&[])).unwrap_err();
        assert!(e.to_string().contains("not a regular file"), "{e}");
    }

    #[test]
    fn token_files_are_size_capped() {
        let dir = tempfile::tempdir().unwrap();
        let ok = token_file(&dir, "ok", &"a".repeat(4096), 0o600);
        let big = token_file(&dir, "big", &"a".repeat(4097), 0o600);
        let at = |p: &PathBuf| {
            cfg(&format!(
                "[telegram]\nbot_token_file = \"{}\"\n",
                p.display()
            ))
            .bot_token(&env_of(&[]))
        };
        assert!(at(&ok).is_ok());
        let e = at(&big).unwrap_err().to_string();
        assert!(e.contains("larger than 4 KiB"), "{e}");
    }

    #[test]
    fn token_files_owned_by_another_user_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let f = token_file(&dir, "t", "secret", 0o600);
        let me = current_uid().expect("uid");
        if std::os::unix::fs::chown(&f, Some(me.wrapping_add(1)), None).is_err() {
            return; // not allowed to chown (not root); nothing to test
        }
        let c = cfg(&format!(
            "[telegram]\nbot_token_file = \"{}\"\n",
            f.display()
        ));
        let e = c.bot_token(&env_of(&[])).unwrap_err().to_string();
        assert!(e.contains("owned by another user"), "{e}");
    }

    fn write_cfg(dir: &tempfile::TempDir, body: &str, mode: u32) -> PathBuf {
        token_file(dir, "gateway.toml", body, mode)
    }

    #[test]
    fn config_with_inline_secrets_must_not_be_group_or_world_readable() {
        let dir = tempfile::tempdir().unwrap();
        for body in [
            "[telegram]\nbot_token = \"123:SECRETVALUE\"\n",
            "[tole]\ntoken = \"SECRETVALUE\"\n",
            "[tole]\nbasic_user = \"u\"\nbasic_password = \"SECRETVALUE\"\n",
        ] {
            let p = write_cfg(&dir, body, 0o644);
            let e = Config::load(&p).unwrap_err();
            assert!(
                matches!(e, ConfigError::InsecurePermissions { .. }),
                "{body}"
            );
            let msg = e.to_string();
            assert!(
                msg.contains("chmod 600") && !msg.contains("SECRETVALUE"),
                "{msg}"
            );
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
            Config::load(&p).unwrap();
        }
        // No inline secret: unrestricted.
        let p = write_cfg(&dir, "[sessions]\nrollover_hours = 2\n", 0o644);
        Config::load(&p).unwrap();
    }

    #[test]
    fn secrets_are_trimmed_from_every_source() {
        let dir = tempfile::tempdir().unwrap();
        let f = token_file(&dir, "t", "  from-file \t\n", 0o600);
        let c = cfg(&format!("[tole]\ntoken_file = \"{}\"\n", f.display()));
        assert_eq!(
            c.tole_auth(&env_of(&[])).unwrap(),
            ToleAuth::Bearer(Secret::new("from-file"))
        );
        let c = cfg("[tole]\ntoken = \" inline \"\n");
        assert_eq!(
            c.tole_auth(&env_of(&[])).unwrap(),
            ToleAuth::Bearer(Secret::new("inline"))
        );
        assert_eq!(
            c.tole_auth(&env_of(&[(ENV_TOLE_TOKEN, "\tenv  ")]))
                .unwrap(),
            ToleAuth::Bearer(Secret::new("env"))
        );
        let c = cfg("[tole]\nbasic_user = \"u\"\nbasic_password = \" p \"\n");
        assert_eq!(
            c.tole_auth(&env_of(&[])).unwrap(),
            ToleAuth::Basic {
                user: "u".into(),
                password: Secret::new("p")
            }
        );
        assert_eq!(
            cfg("[telegram]\nbot_token = \" 1:a \"\n")
                .bot_token(&env_of(&[]))
                .unwrap(),
            Secret::new("1:a")
        );
    }

    #[test]
    fn no_error_contains_a_planted_secret() {
        let secret = "PLANTED-SECRET-123";
        let dir = tempfile::tempdir().unwrap();
        let f = token_file(&dir, "t", secret, 0o644);
        let c = cfg(&format!(
            "[tole]\ntoken = \"{secret}\"\nbasic_user = \"u\"\nbasic_password = \"{secret}\"\n\
             url = \"bad\"\n[telegram]\nbot_token_file = \"{}\"\n",
            f.display()
        ));
        let errs = [
            c.tole_auth(&env_of(&[])).unwrap_err().to_string(),
            c.bot_token(&env_of(&[])).unwrap_err().to_string(),
            c.validate().unwrap_err().to_string(),
            format!("{c:?}"),
        ];
        for e in errs {
            assert!(!e.contains(secret), "{e}");
        }
    }
}
