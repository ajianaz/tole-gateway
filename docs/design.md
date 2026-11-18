# tole-gateway design

Status: the Telegram path (G2 and G3) is implemented and tested against fakes;
not yet run against the real Telegram service. This file records decisions and
the roadmap; update it in the same PR that changes either.

## 1. What it is

A small chat gateway for [`tole serve`](https://github.com/codecoradev/tole), the
HTTP/SSE face of the tole agent harness. It is a **separate process and a thin
client of the tole serve API**: it links no `tole-core` and holds no business
logic. Sessions, tools, approvals, budgets and storage stay in tole.

- First channel: Telegram (one bot, long polling, so no inbound port).
- Second channel: Discord.
- The channel seam (a trait) is extracted only once two adapters exist, not
  before.
- Single user first.

## 2. Decisions

### Authorization

- A chat is authorized by a config allowlist of Telegram user ids.
- First contact from an unknown user uses a one-time pairing code printed on
  the server console. The code is single-use, expires, and is never logged
  anywhere else.
- Messages from any other chat are dropped without a reply.

### Approvals

- A pending approval is shown as inline buttons with the tool and the one-line
  redacted description from tole. The approval row carries no risk field: over
  serve only Write-tier tools can be queued, so the message says so.
- An unanswered approval is a denial.
- Destructive tools are never approvable from chat. tole enforces this; the
  gateway offers no way around it.
- When a prompt stops for an approval, tole serve answers
  `stopReason: approval_required` with the approval row
  ([codecoradev/tole#571](https://github.com/codecoradev/tole/issues/571)).
  The gateway renders that row.

### Sessions

- One chat = one tole session, with one profile per chat.
- A new session starts after **4 hours without a message from the owner**, or
  on `/new`.
  - Only the owner's messages count toward the idle time.
  - A running turn or a pending approval blocks the rollover.
- Older sessions stay in tole's append-only logs. The gateway keeps its own
  chat-to-session index.

### Notifications and cron results

- Cron results and approval notifications arrive through tole's outbound
  signed webhook delivery target (design in the tole repository,
  `docs/webhook-delivery-design.md`, issue
  [codecoradev/tole#911](https://github.com/codecoradev/tole/issues/911)). The
  gateway therefore holds no SSE stream per session; the server caps streams at
  8.
- Until that exists, the gateway polls `GET /approvals?session=` and
  `GET /sessions/{id}/status` after every allow, then reads the new log entries
  for the answer (a resumed turn has no HTTP response). Polling `GET /cron/runs`
  is not implemented yet.
- Incoming deliveries are verified (signature and freshness) before anything
  is shown.

### Secrets

Bot tokens, signing secrets, API keys and pairing codes never appear in
arguments, logs, error messages or tool output.

## 3. Crate layering and module plan

One binary crate (`crates/tole-gateway`) for now. Planned modules, added only
when the roadmap step that needs them lands:

| Module | Role |
|--------|------|
| `config` | Load and validate configuration (allowlist, profiles, tole URL and credentials). |
| `tole_client` | HTTP/SSE client for the tole serve API. No tole-core types. |
| `auth` | Allowlist check and one-time pairing. |
| `sessions` | Chat-to-session index and the 4-hour rollover rule. |
| `approvals` | Render approval rows as buttons; map answers to allow or deny; timeouts are denials. |
| `telegram` | Telegram adapter (long polling, rendering, buttons). |
| `notify` | Webhook receiver and, until it exists, polling of approvals and cron runs. |

When the Discord adapter arrives, the common adapter trait is extracted from
the two concrete adapters and the workspace may split into a library crate plus
the binary.

## 4. Roadmap

| Step | Scope | State |
|------|-------|-------|
| G1 | Repository bootstrap: workspace skeleton, CI and governance checks, docs. | done |
| G2 | Telegram adapter: long polling, allowlist and pairing, one chat = one session, `/new`, 4-hour rollover. | implemented, tested against fakes; live Telegram check pending |
| G3 | Approvals from chat: inline buttons, denial on timeout, polling fallback. | implemented, tested against fakes |
| G4 | Cron results and notifications through the signed webhook delivery target; second channel (Discord) and the adapter seam extraction. | planned |
| G5 | Web dashboard. | later |
