# Receiving tole's webhooks (`[notify]`)

Off by default. When on, the gateway listens for the signed events tole sends
from a `[webhook_out.<name>]` destination (tole `docs/webhook-delivery-design.md`)
so it does not have to poll for them. It is a **notification** channel only: an
event carries no credential and grants nothing, and every approval is still
decided through tole's single gate (`POST /approvals/{id}/decision`).

## What it changes

| Event | What the gateway does |
| --- | --- |
| `approval_requested` | After an Allow, an approval raised by the resumed turn is shown with its buttons at once, without polling `GET /approvals?session=`. The summary is untrusted text, shown as plain text. |
| `approval_resolved` | Remembered, so a late or reordered `approval_requested` for an approval that already closed is dropped (events carry no order). |
| `turn_finished` | Verified like every event, then its ids are checked (the `id` must be `turn:<session>:<hash16>:<last_seq>[:<settle>]` for the `session_id` and `last_seq` in `data`), and the latest end of each session is kept by `(last_seq, at, settle)` (`at` is the envelope time; the settle counter restarts with tole, so it only breaks a tie), so a late older event never replaces a newer one, also across a tole restart. It carries no text. A status or code word the gateway does not know yet is kept as "other" and acknowledged. After an Allow, the end of the resumed turn ends the wait (see below). |
| `cron_run_finished`, `webhook_test`, anything else | Acknowledged (`200`) and ignored: the gateway has no cron presentation yet. |

An event carries a pointer, not the assistant's answer, and it is only a
**hint**: tole keeps these events in a volatile queue (retried for 15 minutes,
lost on a serve restart). After an Allow the gateway waits like this:

1. An `approval_requested` for the session shows the next approval.
2. A `turn_finished` for the session whose `last_seq` is past the log position
   read before the decision (the baseline) ends the wait. At the baseline
   itself only the leg that stopped on the approval just decided is stale; any
   other end there is a resumed leg that failed before writing anything.
   - `completed`: one authenticated read of the log entries after the cursor
     (the baseline, or as far as a fallback poll already read); the last
     assistant message after the baseline is the answer, whether this read or
     an earlier poll found it.
   - `approval_pending`: the announced approval, or one read of
     `GET /approvals?session=` when the announcement was lost.
   - `cancelled`, `detached`, `budget_exhausted`, `failed`, `blocked`: a fixed
     line written by the gateway from the status and code words (never text
     from the event, which has none).
   - an unknown word, or a read that fails: the polls below decide.
3. With no usable event, the old polls (open approvals, `GET
   /sessions/{id}/status`, then the log) run every `fallback_poll_secs`
   (default 15) instead of every second. That is the bound on how long a lost
   event can delay an answer.

When the receiver is off, nothing changes from the one-second polling. List
`turn_finished` in tole's `events` (below): without it every answer after an
Allow waits for the fallback poll.

## Configure the gateway

```toml
[notify]
enabled = true
listen = "127.0.0.1:7811"      # loopback unless allow_non_loopback = true
path = "/tole/events"
destination = "gateway"        # the tole destination name; other names are refused
hmac_key_file = "/etc/tole-gateway/webhook.key"   # chmod 600, or use the env var
max_body_bytes = 65536
fallback_poll_secs = 15
```

The signing key is **never** a config value: set `TOLE_GATEWAY_WEBHOOK_KEY` or
`notify.hmac_key_file` (mode 0600, at most 4 KiB, at least 16 bytes). Use a key
that no other tole destination or hook shares. It is a `Secret`: it prints as
`***` and is never logged.

## Configure tole

```toml
[webhook_out.gateway]
url = "http://127.0.0.1:7811/tole/events"
hmac_key_env = "TOLE_GATEWAY_SECRET"     # the same value as the gateway's key
events = ["approval_requested", "approval_resolved", "turn_finished"]
allow_loopback = true                     # only when the gateway is on the same host
```

## Where it sits: no TLS here

The receiver speaks plain HTTP and **is not a TLS terminator**. It is meant for
one of:

- tole and the gateway on the same host (the default: loopback, nothing leaves
  the machine);
- a private network or tunnel between them; or
- the owner's own reverse proxy (nginx, Caddy) terminating TLS and forwarding to
  the loopback listener. Forward only the one `path`, to the gateway's
  loopback port, and add the proxy's own rate and connection limits.

Binding a non-loopback address needs `allow_non_loopback = true`; do that only
inside a private network or behind such a proxy. The signature authenticates the
sender and the body, but plain HTTP still shows the (redacted) event text to
anyone on the path.

## What every request goes through

1. Not `POST <path>`: `404`. More than 16 requests in flight: `503`.
2. The body is read under a hard cap (`max_body_bytes`, also for chunked
   bodies) and a 10 second deadline: `413` / `408`. A request whose
   `X-Tole-Event` says `turn_finished` is capped at 2 KiB (tole never sends one
   over 1 KiB); a request that lies about its event to get the larger cap fails
   the signature in the next step.
3. The four headers `X-Tole-Timestamp`, `X-Tole-Delivery`, `X-Tole-Event`,
   `X-Tole-Signature` must be present and well formed; the v2 signature
   `HMAC-SHA256(key, "v2\n<ts>\n<id>\n<event>\n<raw body>")` is compared in
   constant time (the pinned tole test vector is a unit test); the timestamp
   must be within 300 seconds. Any failure is the same bare `401`: the answer
   never says which check failed, and nothing a request carries is logged.
4. Only then is the body parsed: `v` is 1, `id` and `type` equal the headers,
   `destination` is the configured name (`400` otherwise).
5. Dedupe on `(destination, id)`, remembered for 24 hours (at most 10 000 ids;
   a full store answers `503` so tole retries, never applies twice). tole
   retries at least once with the same id, so a repeat of an accepted delivery,
   including a captured request replayed, is answered `200` and applied no
   second time.

The dedupe store and the announced approvals are in memory: a gateway restart
forgets them. Replaying a captured request after a restart is still bounded by
the 300 second window, and the worst it can do is announce an approval the
gateway then shows with the same buttons as a polled one.
