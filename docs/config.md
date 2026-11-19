# Configuration

The gateway reads one TOML file (see `config.example.toml` at the repository
root for every key). Unknown keys are rejected. Parse errors report the line
and column but never the offending value.

## Sections

`state_dir` (top level): where the gateway keeps its own state. Default
`$XDG_STATE_HOME/tole-gateway`, else `$HOME/.local/state/tole-gateway`; if none
of these is available, it is an error.

`[tole]`: `url` (default `http://127.0.0.1:7801`, must be http or https with a host, no user name or password; plain http only to loopback),
`token`, `token_file`, `basic_user`, `basic_password`, `profile` (optional,
`[a-z0-9_-]{1,64}`), `cwd` (optional), `request_timeout_secs` (default 30, > 0),
`prompt_timeout_secs` (default 1800, > 0).

`[telegram]`: `bot_token`, `bot_token_file`, `api_base` (default
`https://api.telegram.org`, same URL rules), `poll_timeout_secs` (default 30,
between 1 and 50).

`[auth]`: `allowed_user_ids` (default empty, positive integers),
`pairing_ttl_secs` (default 600, at least 30). An empty allowlist is valid and
means pairing mode.

`[sessions]`: `rollover_hours` (default 4, at least 1).

`[notify]` (off by default, see [notify.md](notify.md)): `enabled` (default
false), `listen` (default `127.0.0.1:7811`; loopback unless `allow_non_loopback`
is true), `path` (default `/tole/events`), `destination` (default `gateway`,
`[a-z0-9_-]{1,32}`), `hmac_key_file`, `max_body_bytes` (default 65536, 1024 to
1048576), `fallback_poll_secs` (default 15, 1 to 3600; how often a resumed turn is
polled while no event arrives). The rules are checked
only when `enabled` is true.

Validation reports all problems in a single error, one per line.

## Environment variables and precedence

| Variable | Overrides |
| --- | --- |
| `TOLE_SERVE_TOKEN` | `tole.token_file`, `tole.token` (bearer token) |
| `TOLE_SERVE_BASIC_USER` | `tole.basic_user` |
| `TOLE_SERVE_BASIC_PASSWORD` | `tole.basic_password` |
| `TOLE_GATEWAY_BOT_TOKEN` | `telegram.bot_token_file`, `telegram.bot_token` |
| `TOLE_GATEWAY_WEBHOOK_KEY` | `notify.hmac_key_file` (the webhook signing key; never an inline value) |
| `XDG_STATE_HOME`, `HOME` | default for `state_dir` |

For each secret the order is: environment, then the `*_file` key, then the
inline value. Every value is trimmed of surrounding whitespace, whatever its
source; a value that is blank after trimming is an error.

For `tole serve`, exactly one scheme must resolve: a bearer token, or a Basic
user plus password. Configuring both, neither, or only half of the Basic pair
is an error.

## Token files

A token file is opened once and checked on that open handle. It must be a
regular file (no FIFO, device or directory) of at most 4 KiB. On unix it must
not be readable by group or others (`mode & 0o077 != 0` is rejected; fix it with
`chmod 600 <file>`) and must be owned by the user running the gateway (checked
when the process uid can be read from `/proc/self`).

The same mode rule applies to the main config file when it holds an inline
secret (`tole.token`, `tole.basic_password` or `telegram.bot_token`). A config
file without inline secrets has no mode requirement.

## Network note

`tole serve` speaks plain HTTP. `tole.url` and `telegram.api_base` may use
`http://` only for loopback hosts (`localhost`, `127.0.0.0/8`, `::1`); any other
`http://` host is rejected because credentials (or the bot token, which is part
of the Bot API path) would cross the network in clear text. If the gateway must
reach serve on another host, put it behind a TLS-terminating proxy or an SSH
tunnel and use an `https://` URL or a loopback URL. URLs must not embed
a user name or password (the `user:password@` part before the host); use the token
keys instead.

Secrets are never printed: errors and `Debug` output show `***` or omit them.
