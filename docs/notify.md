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
| `turn_finished` | Verified like every event, then its ids are checked (the `id` must be `turn:<session>:<hash16>:<last_seq>[:<settle>]` for the `session_id` and `last_seq` in `data`), and the latest end of each session is kept by `(last_seq, settle)`, so a late older event never replaces a newer one. It carries no text. A status or code word the gateway does not know yet is kept as "other" and acknowledged. Not yet used by the dispatcher. |
| `cron_run_finished`, `webhook_test`, anything else | Acknowledged (`200`) and ignored: the gateway has no cron presentation yet. |

An event carries a pointer, not the assistant's answer. tole's `turn_finished`
is received and recorded, but the dispatcher does not act on it yet, so the
gateway still reads `GET /sessions/{id}/status` and the log entries after an
Allow to find the answer; what the receiver removes is the polling for
approvals. While it is on, the open approvals are polled only every
`fallback_poll_secs` (default 15) in case an event was lost. When the receiver
is off, nothing changes from the polling behaviour.

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
events = ["approval_requested", "approval_resolved"]
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
