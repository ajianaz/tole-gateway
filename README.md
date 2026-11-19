# tole-gateway

[![CI](https://github.com/ajianaz/tole-gateway/actions/workflows/ci.yml/badge.svg)](https://github.com/ajianaz/tole-gateway/actions/workflows/ci.yml)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache--2.0-blue.svg)](LICENSE)

A small chat gateway for [`tole serve`](https://github.com/codecoradev/tole).
A separate process and a thin client of the tole serve API: it links no
`tole-core` and holds no business logic. The first channel is Telegram (one
bot, long polling, no inbound port); Discord follows. Single user first.

**Status:** the Telegram path is implemented and tested against fakes: text
messages become tole turns, approvals come back as Allow and Deny buttons,
sessions roll over after four idle hours. It has not run against the real
Telegram service yet; see [docs/e2e.md](docs/e2e.md) for what is proven and what
is not. Roadmap: [docs/design.md](docs/design.md).

## How it works

```
Telegram  <--long polling-->  tole-gateway  <--HTTP, loopback-->  tole serve
```

- One chat is one tole session. A message becomes `POST /sessions/{id}/prompt`
  and the answer comes back to the chat.
- A prompt that needs permission stops with `approval_required`; the gateway
  shows the tool and a one-line description with **Allow** and **Deny**
  buttons. The decision goes through tole's single approval gate. An approval
  you do not answer is denied.
- Optionally (`[notify]`, off by default) tole's signed outbound webhooks tell
  the gateway about approvals raised by a resumed turn, so it need not poll for
  them; polling remains the fallback.
- A new session starts after 4 hours without a message from you, or on `/new`.
- Only chats on the allowlist, or one that has just entered the one-time
  pairing code, reach tole. Everyone else gets no reply at all.

## Quickstart

1. **Run tole serve** on the same machine (loopback only, with a token). Give
   it your LLM provider as usual (`TOLE_BASE_URL`, `TOLE_API_KEY`,
   `TOLE_MODEL`):

   ```bash
   umask 077 && openssl rand -hex 32 > ~/.config/tole-serve-token
   tole serve --bind 127.0.0.1 --port 7801 --token-file ~/.config/tole-serve-token
   ```

2. **Create a Telegram bot.** Talk to [@BotFather](https://t.me/BotFather),
   send `/newbot`, and keep the token it gives you.

3. **Write the gateway config** (every key is explained in
   [docs/config.md](docs/config.md)); start from the example:

   ```bash
   mkdir -p ~/.config/tole-gateway
   cp config.example.toml ~/.config/tole-gateway/config.toml
   ```

   Keep secrets out of the file: put the bot token in a file readable only by
   you, or in the environment.

   ```bash
   export TOLE_GATEWAY_BOT_TOKEN='123456:...'      # from BotFather
   export TOLE_SERVE_TOKEN="$(cat ~/.config/tole-serve-token)"
   ```

4. **Check, then run:**

   ```bash
   tole-gateway --config ~/.config/tole-gateway/config.toml --check
   tole-gateway --config ~/.config/tole-gateway/config.toml
   ```

5. **Authorize yourself.** Either list your Telegram user id in
   `auth.allowed_user_ids`, or leave the list empty: the gateway then prints a
   one-time pairing code on its console. Send that code to the bot in a private
   chat. Pairing lasts for this run only; the reply tells you the id to add to
   the config to keep access after a restart.

## Commands

| Command | What it does |
| --- | --- |
| `/new` | Start a fresh session (refused while a turn or an approval is pending). |
| `/cancel` | Stop the running turn, or deny a pending approval. |
| `/status` | Show whether the session is working or idle. |
| `/help` | List the commands. |

## Security model

- Allowlist or pairing only; messages from anyone else are dropped silently.
- Approvals go only through tole's gate. The gateway never approves on its own
  and has no code path that approves without your button press. Destructive
  tools cannot be approved from chat (tole refuses them over serve).
- Tokens and the pairing code never appear in logs, errors or chat messages.
  The pairing code is shown only on the console.
- Keep tole serve on loopback. It speaks plain HTTP; do not expose it, and do
  not run the gateway on a different host without a TLS-terminating proxy.
- Run one gateway process per state directory (the session index has no lock).

## Build

```bash
cargo build --workspace --release
./target/release/tole-gateway --version
```

## Documentation

- [docs/config.md](docs/config.md): every configuration key and environment variable.
- [docs/notify.md](docs/notify.md): the optional receiver for tole's signed webhooks.
- [docs/design.md](docs/design.md): decisions, module plan and roadmap.
- [docs/e2e.md](docs/e2e.md): end-to-end tests against a real `tole serve`.
- [CONTRIBUTING.md](CONTRIBUTING.md): workflow, branches, PRs, review gate.
- [docs/repo-settings.md](docs/repo-settings.md): owner checklist for repository settings.
- [SECURITY.md](SECURITY.md): how to report a vulnerability.

## License

Apache-2.0, see [LICENSE](LICENSE).
