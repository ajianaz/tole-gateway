//! `tole-gateway`: a chat gateway for `tole serve`.
//!
//! The crate layering and module plan live in `docs/design.md`.

use std::path::PathBuf;
use std::process::ExitCode;

use tole_gateway::runtime::run_gateway;

const HELP: &str = "\
tole-gateway: chat gateway for `tole serve` (Telegram)

USAGE:
    tole-gateway --config <FILE> [--check]

OPTIONS:
    -c, --config <FILE>   Path to the gateway configuration (TOML)
        --check           Validate the configuration, resolve the credentials and
                          probe tole, then exit
    -h, --help            Print this help and exit
    -V, --version         Print the version and exit

Secrets come from the environment (TOLE_SERVE_TOKEN, TOLE_GATEWAY_BOT_TOKEN) or
from files readable only by you; see docs/config.md. Run it in the foreground
or under a supervisor; stop it with Ctrl-C or SIGTERM.
";

/// What the command line asked for.
#[derive(Debug, PartialEq, Eq)]
enum Action {
    Help,
    Version,
    Run {
        config: PathBuf,
        check: bool,
    },
    /// An argument that is not understood, or a missing value.
    Unknown(String),
}

fn parse<I: IntoIterator<Item = String>>(args: I) -> Action {
    let mut args = args.into_iter();
    let mut config: Option<PathBuf> = None;
    let mut check = false;
    let mut seen_any = false;
    while let Some(arg) = args.next() {
        seen_any = true;
        match arg.as_str() {
            "-h" | "--help" => return Action::Help,
            "-V" | "--version" => return Action::Version,
            "--check" => check = true,
            "-c" | "--config" => match args.next() {
                Some(path) => config = Some(PathBuf::from(path)),
                None => return Action::Unknown(format!("{arg} needs a value")),
            },
            other => return Action::Unknown(other.to_string()),
        }
    }
    match (seen_any, config) {
        (false, _) => Action::Help,
        (true, Some(config)) => Action::Run { config, check },
        (true, None) => Action::Unknown("--config is required".to_string()),
    }
}

fn main() -> ExitCode {
    match parse(std::env::args().skip(1)) {
        Action::Help => {
            print!("{HELP}");
            ExitCode::SUCCESS
        }
        Action::Version => {
            println!("tole-gateway {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Action::Unknown(arg) => {
            eprintln!("tole-gateway: {arg}\n\n{HELP}");
            ExitCode::from(2)
        }
        Action::Run { config, check } => run(config, check),
    }
}

fn run(config: PathBuf, check: bool) -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("tole-gateway: cannot start the async runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    let env = |key: &str| std::env::var(key).ok();
    match runtime.block_on(run_gateway(&config, check, &env)) {
        Ok(report) => {
            if check {
                println!("tole-gateway: configuration ok");
                println!(
                    "tole-gateway: tole serve {}",
                    if report.tole_reachable {
                        "is reachable"
                    } else {
                        "is NOT reachable (the gateway would keep retrying)"
                    }
                );
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("tole-gateway: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_help_version_and_unknown() {
        assert_eq!(parse(args(&[])), Action::Help);
        assert_eq!(parse(args(&["--help"])), Action::Help);
        assert_eq!(parse(args(&["-V"])), Action::Version);
        assert_eq!(parse(args(&["--version"])), Action::Version);
        assert_eq!(parse(args(&["--nope"])), Action::Unknown("--nope".into()));
    }

    #[test]
    fn parses_run_and_check() {
        assert_eq!(
            parse(args(&["--config", "g.toml"])),
            Action::Run {
                config: "g.toml".into(),
                check: false
            }
        );
        assert_eq!(
            parse(args(&["--check", "-c", "g.toml"])),
            Action::Run {
                config: "g.toml".into(),
                check: true
            }
        );
    }

    #[test]
    fn a_missing_config_or_value_is_an_error() {
        assert!(matches!(parse(args(&["--check"])), Action::Unknown(_)));
        assert!(matches!(parse(args(&["--config"])), Action::Unknown(_)));
    }
}
