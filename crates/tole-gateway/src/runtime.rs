//! Startup: load the configuration, build the clients, start the app.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tracing::{info, warn};

use crate::app::{Announce, App, AppSettings};
use crate::auth::{Authorizer, OsPairingCodeSource};
use crate::channel::UserId;
use crate::config::{Config, EnvFn};
use crate::error::ToleError;
use crate::notify::{self, NotifyHub, Receiver, ReceiverConfig};
use crate::sessions::{SessionIndex, SystemClock};
use crate::telegram::{TelegramAdapter, TelegramConfig};
use crate::tole_client::{HttpToleClient, ToleApi, ToleClientConfig};

/// What `--check` found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckReport {
    /// Whether `tole serve` answered `GET /health`.
    pub tole_reachable: bool,
}

/// Load and validate the configuration, resolve every credential, and probe
/// tole. With `check_only` it stops there; otherwise it runs the gateway until
/// the process is told to stop.
pub async fn run_gateway(
    config_path: &Path,
    check_only: bool,
    env: EnvFn<'_>,
) -> Result<CheckReport, String> {
    let cfg = Config::load(config_path).map_err(|e| e.to_string())?;
    cfg.validate().map_err(|e| e.to_string())?;
    let tole_auth = cfg.tole_auth(env).map_err(|e| e.to_string())?;
    let bot_token = cfg.bot_token(env).map_err(|e| e.to_string())?;
    let state_dir = cfg.state_dir(env).map_err(|e| e.to_string())?;
    // Off by default: nothing is read or bound unless `[notify] enabled`.
    let webhook_key = if cfg.notify.enabled {
        Some(cfg.webhook_key(env).map_err(|e| e.to_string())?)
    } else {
        None
    };

    let tole = HttpToleClient::new(ToleClientConfig {
        base_url: cfg.tole.url.clone(),
        auth: tole_auth,
        request_timeout: Duration::from_secs(cfg.tole.request_timeout_secs),
        prompt_timeout: Duration::from_secs(cfg.tole.prompt_timeout_secs),
    })
    .map_err(|e| e.to_string())?;

    let tole_reachable = tole.health().await.is_ok();
    if !tole_reachable {
        warn!(
            "tole serve is not reachable at {}; the gateway keeps running and will answer chats \
             with an error until it is",
            cfg.tole.url
        );
    }
    // A rejected token is fatal: nothing will ever work.
    if let Err(ToleError::Unauthorized) = tole.approvals(None).await {
        return Err("tole rejected the configured credentials".to_owned());
    }

    let adapter = TelegramAdapter::new(TelegramConfig {
        api_base: cfg.telegram.api_base.clone(),
        bot_token,
        poll_timeout: Duration::from_secs(cfg.telegram.poll_timeout_secs),
    })
    .map_err(|e| e.to_string())?;

    if check_only {
        return Ok(CheckReport { tole_reachable });
    }

    if let Err(e) = adapter.register_commands().await {
        warn!("could not register the bot commands: {e}");
    }

    std::fs::create_dir_all(&state_dir).map_err(|e| {
        format!(
            "cannot create the state directory {}: {e}",
            state_dir.display()
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o700));
    }
    let index = SessionIndex::open(state_dir.join("sessions.json")).map_err(|e| e.to_string())?;

    let settings = AppSettings::from_config(&cfg);
    let pairing_mode = cfg.auth.allowed_user_ids.is_empty();
    let auth = Authorizer::new(
        cfg.auth.allowed_user_ids.iter().copied().map(UserId),
        settings.pairing_ttl,
        SystemClock,
        OsPairingCodeSource,
    );
    let ttl_minutes = settings.pairing_ttl.as_secs().div_ceil(60);
    // The one place a pairing code is shown: the operator's console.
    let announce: Announce = Arc::new(move |code| {
        eprintln!(
            "tole-gateway: pairing code {} (valid {ttl_minutes} min). Send it to the bot in a \
             private chat to pair.",
            code.reveal()
        );
    });

    info!(
        "gateway starting (profile: {}, rollover: {} h, allowlisted users: {})",
        cfg.tole.profile.as_deref().unwrap_or("none"),
        cfg.sessions.rollover_hours,
        cfg.auth.allowed_user_ids.len()
    );
    let mut app = App::new(
        tole,
        adapter.clone(),
        SystemClock,
        settings,
        auth,
        index,
        pairing_mode,
        announce,
    );
    let stop = Arc::new(tokio::sync::Notify::new());
    let mut receiver_task = None;
    if let (Some(key), Some(addr)) = (webhook_key, cfg.notify_addr()) {
        let hub = Arc::new(NotifyHub::new(Duration::from_secs(
            cfg.notify.fallback_poll_secs,
        )));
        let listener = notify::bind(addr)
            .await
            .map_err(|e| format!("cannot bind the webhook receiver on {addr}: {e}"))?;
        let receiver = Receiver::new(
            ReceiverConfig {
                key,
                path: cfg.notify.path.clone(),
                destination: cfg.notify.destination.clone(),
                max_body_bytes: cfg.notify.max_body_bytes,
            },
            Arc::clone(&hub),
        );
        let wait = Arc::clone(&stop);
        receiver_task = Some(tokio::spawn(async move {
            let done = async move { wait.notified().await };
            if let Err(e) = receiver.serve(listener, done).await {
                warn!("the webhook receiver stopped: {e}");
            }
        }));
        app = app.with_notifications(hub);
    }
    let app = Arc::new(app);
    app.run(&adapter, shutdown_signal()).await;
    stop.notify_one();
    if let Some(t) = receiver_task {
        let _ = t.await;
    }
    Ok(CheckReport { tole_reachable })
}

/// Completes on Ctrl-C or, on unix, SIGTERM.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = signal(SignalKind::terminate()).ok();
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = async {
                match term.as_mut() {
                    Some(t) => { t.recv().await; }
                    None => std::future::pending::<()>().await,
                }
            } => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
