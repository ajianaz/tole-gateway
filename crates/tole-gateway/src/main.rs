//! `tole-gateway`: a chat gateway for `tole serve`.
//!
//! Bootstrap stage: the binary only answers `--version` and `--help`.
//! The crate layering and module plan live in `docs/design.md`.

use std::process::ExitCode;

const HELP: &str = "\
tole-gateway: chat gateway for `tole serve`

USAGE:
    tole-gateway [OPTIONS]

OPTIONS:
    -h, --help       Print this help and exit
    -V, --version    Print the version and exit
";

/// What the command line asked for.
#[derive(Debug, PartialEq, Eq)]
enum Action {
    Help,
    Version,
    /// An argument this stage does not know.
    Unknown(String),
}

fn parse<I: IntoIterator<Item = String>>(args: I) -> Action {
    match args.into_iter().next().as_deref() {
        None | Some("-h") | Some("--help") => Action::Help,
        Some("-V") | Some("--version") => Action::Version,
        Some(other) => Action::Unknown(other.to_string()),
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
            eprintln!("tole-gateway: unknown argument: {arg}\n\n{HELP}");
            ExitCode::from(2)
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
}
