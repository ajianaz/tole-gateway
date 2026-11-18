# Configuration

The gateway reads one TOML file (see `config.example.toml` at the repository
root for every key). Unknown keys are rejected. Parse errors report the line
and column but never the offending value.

## Sections

`state_dir` (top level): where the gateway keeps its own state. Default
`$XDG_STATE_HOME/tole-gateway`, else `$HOME/.local/state/tole-gateway`; if none
of these is available, it is an error.

`[tole]`: `url` (default `http://127.0.0.1:7801`, must be http or https),
`token`, `token_file`, `basic_user`, `basic_password`, `profile` (optional,
`[a-z0-9_-]{1,64}`), `cwd` (optional), `request_timeout_secs` (default 30, > 0),
`prompt_timeout_secs` (default 1800, > 0).

`[telegram]`: `bot_token`, `bot_token_file`, `api_base` (default
`https://api.telegram.org`, http or https), `poll_timeout_secs` (default 30,
between 1 and 50).

`[auth]`: `allowed_user_ids` (default empty, positive integers),
`pairing_ttl_secs` (default 600, at least 30). An empty allowlist is valid and
means pairing mode.

`[sessions]`: `rollover_hours` (default 4, at least 1).

Validation reports all problems in a single error, one per line.

## Environment variables and precedence

| Variable | Overrides |
| --- | --- |
| `TOLE_SERVE_TOKEN` | `tole.token_file`, `tole.token` (bearer token) |
| `TOLE_SERVE_BASIC_USER` | `tole.basic_user` |
| `TOLE_SERVE_BASIC_PASSWORD` | `tole.basic_password` |
| `TOLE_GATEWAY_BOT_TOKEN` | `telegram.bot_token_file`, `telegram.bot_token` |
| `XDG_STATE_HOME`, `HOME` | default for `state_dir` |

For each secret the order is: environment, then the `*_file` key, then the
inline value. Blank values are errors, at any level. Token files have one
trailing newline removed.

For `tole serve`, exactly one scheme must resolve: a bearer token, or a Basic
user plus password. Configuring both, neither, or only half of the Basic pair
is an error.

## Token files

On unix a token file must not be readable by group or others. A file with
`mode & 0o077 != 0` is rejected; fix it with `chmod 600 <file>`.

## Network note

`tole serve` speaks plain HTTP. Keep it on `127.0.0.1` (the default) so the
bearer token never crosses a network. If the gateway must reach serve on
another host, put it behind a TLS-terminating proxy or an SSH tunnel and point
`tole.url` at that.

Secrets are never printed: errors and `Debug` output show `***` or omit them.
