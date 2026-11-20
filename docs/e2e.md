# End-to-end tests against a real `tole serve`

The unit and integration tests of this crate run against fakes (a fake
`tole serve`, a fake Telegram Bot API). `crates/tole-gateway/tests/e2e.rs`
runs the gateway's real pieces against the real `tole` binary:

- `HttpToleClient`, `TelegramAdapter` and `App` (with its run loop) are the
  production code;
- the Telegram Bot API is `tests/support/fake_telegram.rs`;
- `tole serve` is the real process; its LLM is
  `tests/support/fake_provider.rs`, an OpenAI-compatible `/chat/completions`
  server on loopback (no network, no key);
- for the `webhook_*` tests, tole also has a `[webhook_out.gateway]`
  destination pointing at the gateway's real receiver (`notify::Receiver`
  and `NotifyHub`, on an ephemeral loopback port bound by the test before tole
  starts). The test writes tole's user config (`$CODECORA_HOME/tole/config.toml`)
  with `allow_loopback = true`, `hmac_key_env = "TOLE_E2E_WEBHOOK_KEY"` and an
  `events` list, and passes the key in tole's environment only.

## Running

Build tole once (the tests never build or modify it), then:

```text
TOLE_BIN=/path/to/tole/target/debug/tole \
  cargo test -p tole-gateway --test e2e -- --ignored --test-threads=1
```

Every test is `#[ignore = "needs TOLE_BIN"]`, so a plain
`cargo test --workspace` neither needs tole nor runs these. Tested against
tole 0.11.0.

Each test starts its own `tole serve` on `127.0.0.1:<free port>` with
`--token-file`, `--no-auto-mcp`, a temp workspace and a temp `--sessions-dir`.
The child gets `env_clear()` plus only `PATH`, `HOME`, `CODECORA_HOME`, the
`XDG_*` dirs, `TMPDIR`, `TOLE_CRON_DIR`, `TOLE_NO_UPDATE_CHECK` and
`TOLE_BASE_URL`/`TOLE_API_KEY`/`TOLE_MODEL`/`TOLE_STREAM=0` (and `TOLE_E2E_WEBHOOK_KEY`, the test signing key), so no proxy
variable, user config or real key reaches it. No `--allow` is given, so every
Write tool call parks in the approval queue. The test waits for `GET /health`,
kills the child on drop, and prints the child's stderr when a test fails.

## What the tests prove

| Test | Scenario |
|---|---|
| `a_plain_message_is_answered_in_the_chat` | message in, `echo: ...` back; the provider got its key |
| `approval_allow_writes_the_file_and_the_final_answer_reaches_the_chat` | `WRITE notes.md` queues an approval, the chat shows Allow/Deny, Allow resumes tole, the file is written in the workspace and `done: notes.md` reaches the chat (the wait-after-allow logic against the real server) |
| `approval_deny_leaves_the_file_unwritten_and_says_so` | Deny: no file, the chat says so, the session still answers afterwards |
| `the_same_session_is_reused_for_a_second_message` | one session file on disk, the second request carries the first turn |
| `the_gateway_recovers_after_tole_restarts` | kill tole, start a new process on the same sessions dir and port: the gateway resumes the same session (history kept) without telling the user |
| `cancel_stops_a_slow_turn` | `/cancel` during a 2 s provider delay ends the turn as `Cancelled.`; the session works afterwards |
| `the_real_wire_shapes_match_what_the_gateway_assumes` | raw HTTP checks of the shapes below |
| `webhook_events_carry_a_whole_two_approval_flow_without_polling` | `WRITE first.md ALSO second.md`: the second approval, raised by the resumed turn, reaches the chat as `approval_requested`, and after the second Allow `turn_finished` ends the wait with `done: second.md`. The gateway's fallback poll is 600 s, so an answer inside the 40 s wait can only be an event (with `turn_finished` left out of tole's `events` this test times out). The hub also holds the `approval_resolved` ids and a `completed` turn end |
| `webhook_forged_and_stale_deliveries_are_dropped_while_the_real_flow_runs` | raw POSTs to the receiver during the real flow: wrong key, stale timestamp with the right key (what a replay looks like), no headers, tampered body. Each is a bare `401`; the chat shows no forged approval, the hub keeps no forged approval or turn end, and the real flow completes |
| `webhook_receiver_off_polling_still_completes` | tole has the destination but nothing listens (gateway started without `[notify]`): the same two-approval flow completes by polling |
| `webhook_without_turn_finished_the_fallback_poll_answers` | receiver on, tole not told to send `turn_finished`, `fallback_poll_secs = 1`: the answer after an Allow comes from the fallback poll; `approval_resolved` still arrives |

## Verified wire facts (tole 0.11.0)

- Final assistant message in `GET /sessions/{id}/entries`: an entry with
  `type: "message"`, `payload: {"role": "assistant", "text": "<string>"}`.
  Between turns the log also holds `error`, `intent`, `tool_result` entries and
  a user-role `denial_note` message; `seq` is increasing and not dense.
- `POST /sessions/{id}/prompt` that parks on a Write answers `200
  {"stopReason":"approval_required","approval":{id, sessionId, tool,
  description, status:"pending", createdAtMs, expiresInSeconds:900}}`. The row
  equals the one in `GET /approvals?session=<id>`. A decided row stays in the
  list with status `approved` or `denied`.
- Allow answers `202 {"id","status":"approved","resuming":true}`. The resumed
  turn first appends a user-role denial note (`denial_note: true`, "[tole] Your
  call to `write_file` was denied ..."): the parked call is not re-run by tole
  itself, the MODEL must repeat the identical call, which the one-shot allow
  then lets through. The status shows `busy: true` while it runs and its
  answer lands in the log.
- Ids (`srv-<hex>-<n>-<hex>`, `apr-<hex>-<hex>`) contain only unreserved URL
  characters, so the client's percent-encoding leaves them unchanged.
- After a tole restart a prompt to the old session id answers **500**
  `{"error":"unknown session: <id>"}` (the docs and other routes say 404; the
  gateway treats both as a lost session). `POST /sessions {"resume": id}` then
  answers 200 `resumed: true`, the log (a `<id>.jsonl` file in the sessions
  dir) is intact; resuming an id with no file is 404.

## Limits

- The webhook receiver ([notify.md](notify.md)) is tested against a fake tole
  that plays the signer (`tests/app.rs`, `tests/notify.rs`) and, in the
  `webhook_*` tests above, against the real `tole serve` signing and sending
  to it. Not covered there: tole's retry of a lost delivery and a verbatim
  replay of a captured request (the receiver's dedupe is unit tested; the real
  run only replays with a stale timestamp), a tole restart (its event queue is
  volatile), and `cron_run_finished` (not handled by the gateway yet).
- No real Telegram: the Bot API is a fake, so message formatting limits, rate
  limits, long polling behavior and button rendering on a phone are not
  exercised.
- No real model: the fake provider is stateless and deterministic. How a real
  model reacts to tole's denial note after an allow (it must repeat the call)
  is not covered; a model that answers with text instead would make the
  gateway report that text without writing the file.
- Single owner, one chat, one tole process; no concurrency stress, no 15
  minute approval expiry, no profiles, no TLS or Basic auth.
