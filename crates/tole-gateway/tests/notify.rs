//! The webhook receiver, over real HTTP: signature, window, replay, dedupe,
//! body cap, defaults and the leak check.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tole_gateway::config::{Config, ConfigError, ENV_WEBHOOK_KEY};
use tole_gateway::notify::{
    sign, NotifyHub, Receiver, ReceiverConfig, TurnCode, TurnStatus, MAX_TURN_BODY,
};
use tole_gateway::secret::Secret;

const KEY: &str = "tole-test-key-0123456789abcdef";
const NOW: u64 = 1_795_000_000;
const PATH: &str = "/tole/events";

struct Rig {
    url: String,
    hub: Arc<NotifyHub>,
    clock: Arc<AtomicU64>,
    http: reqwest::Client,
}

async fn rig() -> Rig {
    rig_with(64 * 1024).await
}

async fn rig_with(max_body_bytes: usize) -> Rig {
    let hub = Arc::new(NotifyHub::new(Duration::from_secs(15)));
    let clock = Arc::new(AtomicU64::new(NOW));
    let c = Arc::clone(&clock);
    let receiver = Receiver::with_clock(
        ReceiverConfig {
            key: Secret::new(KEY),
            path: PATH.into(),
            destination: "gateway".into(),
            max_body_bytes,
        },
        Arc::clone(&hub),
        Arc::new(move || c.load(Ordering::SeqCst)),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(receiver.serve(listener, std::future::pending()));
    Rig {
        url,
        hub,
        clock,
        http: reqwest::Client::new(),
    }
}

fn requested(approval: &str, session: &str) -> (String, String, Vec<u8>) {
    let id = format!("approval:{approval}:requested");
    let body = json!({
        "v": 1, "id": id, "type": "approval_requested", "destination": "gateway",
        "at": NOW,
        "data": {
            "source": "serve", "approval_id": approval, "session_id": session,
            "tool": "write_file", "risk": "write", "summary": "write notes.md",
            "truncated": false, "created_at": NOW, "expires_at": NOW + 900
        }
    });
    (
        id,
        "approval_requested".into(),
        serde_json::to_vec(&body).unwrap(),
    )
}

struct Req {
    ts: String,
    id: String,
    event: String,
    sig: Option<String>,
    body: Vec<u8>,
}

impl Req {
    fn signed(id: &str, event: &str, body: &[u8], ts: u64) -> Self {
        let ts = ts.to_string();
        Self {
            sig: Some(sign(&Secret::new(KEY), &ts, id, event, body)),
            ts,
            id: id.into(),
            event: event.into(),
            body: body.to_vec(),
        }
    }
}

async fn send(r: &Rig, q: &Req) -> (u16, String) {
    let mut b = r
        .http
        .post(format!("{}{PATH}", r.url))
        .header("X-Tole-Timestamp", &q.ts)
        .header("X-Tole-Delivery", &q.id)
        .header("X-Tole-Event", &q.event)
        .body(q.body.clone());
    if let Some(s) = &q.sig {
        b = b.header("X-Tole-Signature", s);
    }
    let resp = b.send().await.unwrap();
    (resp.status().as_u16(), resp.text().await.unwrap())
}

fn announced(r: &Rig, session: &str) -> Option<String> {
    r.hub
        .take_requested(session, &[], NOW * 1000)
        .map(|row| row.id)
}

#[tokio::test]
async fn a_valid_delivery_is_accepted_and_applied() {
    let r = rig().await;
    let (id, ev, body) = requested("apr-1", "srv-1");
    assert_eq!(
        send(&r, &Req::signed(&id, &ev, &body, NOW)).await,
        (200, String::new())
    );
    assert_eq!(announced(&r, "srv-1").as_deref(), Some("apr-1"));
}

#[tokio::test]
async fn every_refusal_looks_the_same_and_applies_nothing() {
    let r = rig().await;
    let (id, ev, body) = requested("apr-1", "srv-1");
    let good = Req::signed(&id, &ev, &body, NOW);

    let mut bad_sig = Req::signed(&id, &ev, &body, NOW);
    // Change the last hex digit to a DIFFERENT one: appending a fixed '0'
    // would leave a signature that ends in '0' unchanged (1 run in 16).
    bad_sig.sig = bad_sig.sig.map(|s| {
        let (head, last) = s.split_at(s.len() - 1);
        let other = if last == "0" { "1" } else { "0" };
        format!("{head}{other}")
    });
    let mut missing_sig = Req::signed(&id, &ev, &body, NOW);
    missing_sig.sig = None;
    let mut missing_ts = Req::signed(&id, &ev, &body, NOW);
    missing_ts.ts = String::new();
    let old = Req::signed(&id, &ev, &body, NOW - 301);
    let future = Req::signed(&id, &ev, &body, NOW + 301);
    let mut tampered = Req::signed(&id, &ev, &body, NOW);
    tampered.body = body
        .iter()
        .map(|b| if *b == b'w' { b'W' } else { *b })
        .collect();
    let mut wrong_event = Req::signed(&id, &ev, &body, NOW);
    wrong_event.event = "approval_resolved".into();
    let mut wrong_id = Req::signed(&id, &ev, &body, NOW);
    wrong_id.id = "approval:apr-9:requested".into();
    let mut other_key = Req::signed(&id, &ev, &body, NOW);
    other_key.sig = Some(sign(
        &Secret::new("a-different-key-0123456789"),
        &good.ts,
        &id,
        &ev,
        &body,
    ));

    for (name, q) in [
        ("bad signature", bad_sig),
        ("missing signature", missing_sig),
        ("missing timestamp", missing_ts),
        ("old timestamp", old),
        ("future timestamp", future),
        ("tampered body", tampered),
        ("wrong event header", wrong_event),
        ("wrong id header", wrong_id),
        ("other key", other_key),
    ] {
        assert_eq!(send(&r, &q).await, (401, String::new()), "{name}");
    }
    assert_eq!(announced(&r, "srv-1"), None, "nothing was applied");
    // The untouched request still passes afterwards: nothing was consumed.
    assert_eq!(send(&r, &good).await.0, 200);
}

#[tokio::test]
async fn a_window_edge_is_accepted_and_one_second_more_is_not() {
    let r = rig().await;
    let (id, ev, body) = requested("apr-1", "srv-1");
    assert_eq!(
        send(&r, &Req::signed(&id, &ev, &body, NOW - 300)).await.0,
        200
    );
    let (id2, ev2, body2) = requested("apr-2", "srv-1");
    assert_eq!(
        send(&r, &Req::signed(&id2, &ev2, &body2, NOW - 301))
            .await
            .0,
        401
    );
}

#[tokio::test]
async fn a_replay_is_acknowledged_but_applied_once() {
    let r = rig().await;
    let (id, ev, body) = requested("apr-1", "srv-1");
    let q = Req::signed(&id, &ev, &body, NOW);
    assert_eq!(send(&r, &q).await.0, 200);
    assert_eq!(announced(&r, "srv-1").as_deref(), Some("apr-1"));
    // The identical request again, then a tole retry with a fresh timestamp
    // and signature but the same delivery id.
    assert_eq!(send(&r, &q).await.0, 200);
    r.clock.fetch_add(60, Ordering::SeqCst);
    let retry = Req::signed(&id, &ev, &body, NOW + 60);
    assert_eq!(send(&r, &retry).await.0, 200);
    assert_eq!(announced(&r, "srv-1"), None, "the repeats applied nothing");
}

#[tokio::test]
async fn an_event_that_arrives_after_its_resolution_is_dropped() {
    let r = rig().await;
    let id = "approval:apr-1:resolved";
    let resolved = json!({"v":1,"id":id,"type":"approval_resolved","destination":"gateway","at":NOW,
        "data":{"approval_id":"apr-1","session_id":"srv-1","tool":"write_file","status":"expired","resolved_at":NOW}});
    let rb = serde_json::to_vec(&resolved).unwrap();
    assert_eq!(
        send(&r, &Req::signed(id, "approval_resolved", &rb, NOW))
            .await
            .0,
        200
    );
    let (rid, ev, body) = requested("apr-1", "srv-1");
    assert_eq!(send(&r, &Req::signed(&rid, &ev, &body, NOW)).await.0, 200);
    assert_eq!(announced(&r, "srv-1"), None, "events carry no order");
}

#[tokio::test]
async fn authenticated_but_inconsistent_bodies_are_refused_without_effect() {
    let r = rig().await;
    let (id, ev, body) = requested("apr-1", "srv-1");
    let mut doc: Value = serde_json::from_slice(&body).unwrap();
    doc["destination"] = json!("someone-else");
    let b = serde_json::to_vec(&doc).unwrap();
    assert_eq!(send(&r, &Req::signed(&id, &ev, &b, NOW)).await.0, 400);
    let mut doc: Value = serde_json::from_slice(&body).unwrap();
    doc["v"] = json!(2);
    let b = serde_json::to_vec(&doc).unwrap();
    assert_eq!(send(&r, &Req::signed(&id, &ev, &b, NOW)).await.0, 400);
    assert_eq!(
        send(&r, &Req::signed(&id, &ev, b"not json", NOW)).await.0,
        400
    );
    assert_eq!(announced(&r, "srv-1"), None);
}

#[tokio::test]
async fn events_the_gateway_does_not_act_on_are_acknowledged() {
    let r = rig().await;
    let id = "cron:daily-1:0123456789abcdef";
    let b = json!({"v":1,"id":id,"type":"cron_run_finished","destination":"gateway","at":NOW,"data":{}});
    let b = serde_json::to_vec(&b).unwrap();
    assert_eq!(
        send(&r, &Req::signed(id, "cron_run_finished", &b, NOW))
            .await
            .0,
        200
    );
}

#[tokio::test]
async fn the_body_is_capped() {
    let r = rig_with(2048).await;
    let (id, ev, body) = requested("apr-1", "srv-1");
    let mut padded: Value = serde_json::from_slice(&body).unwrap();
    padded["data"]["summary"] = json!("x".repeat(4096));
    let big = serde_json::to_vec(&padded).unwrap();
    // Declared length over the cap, even though the signature would be fine.
    let (status, text) = send(&r, &Req::signed(&id, &ev, &big, NOW)).await;
    assert_eq!((status, text.as_str()), (413, ""));

    // A chunked body has no declared length: the cap still stops it.
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let addr = r.url.trim_start_matches("http://").to_owned();
    let mut s = tokio::net::TcpStream::connect(&addr).await.unwrap();
    let chunk = vec![b'a'; 8192];
    let head = format!(
        "POST {PATH} HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n",
        chunk.len()
    );
    s.write_all(head.as_bytes()).await.unwrap();
    let _ = s.write_all(&chunk).await;
    let _ = s.write_all(b"\r\n0\r\n\r\n").await;
    let mut out = String::new();
    let _ = s.read_to_string(&mut out).await;
    assert!(out.starts_with("HTTP/1.1 413"), "{out}");
    assert_eq!(announced(&r, "srv-1"), None);
}

#[tokio::test]
async fn other_paths_and_methods_are_a_plain_404() {
    let r = rig().await;
    for (m, p) in [
        ("GET", PATH),
        ("PUT", PATH),
        ("POST", "/"),
        ("POST", "/tole/events/x"),
    ] {
        let resp = r
            .http
            .request(m.parse().unwrap(), format!("{}{p}", r.url))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 404, "{m} {p}");
        assert_eq!(resp.text().await.unwrap(), "");
    }
}

// -------------------------------------------------------- turn_finished

/// tole's second pinned vector (`webhook_out/turn_tests.rs`): these exact bytes
/// signed with [`KEY`] at [`NOW`].
const TURN_ID: &str = "turn:s-1f2e3d:357319c7c674b505:14:1";
const TURN_BODY: &str = r#"{"at":1795000000,"data":{"last_seq":14,"session_id":"s-1f2e3d","status":"completed"},"destination":"gateway","id":"turn:s-1f2e3d:357319c7c674b505:14:1","type":"turn_finished","v":1}"#;
const TURN_SIG: &str = "v2=40b6e20fcf55ad9521b43149ff2314ff1c852b65d8e1463d3b1d23c84270c168";

/// A `turn_finished` body for `session` "s-1f2e3d" at `(last_seq, settle)`.
fn turn(last_seq: u64, settle: u64, data: Value) -> (String, Vec<u8>) {
    let id = format!("turn:s-1f2e3d:357319c7c674b505:{last_seq}:{settle}");
    let mut d = json!({"session_id": "s-1f2e3d", "status": "completed", "last_seq": last_seq});
    for (k, v) in data.as_object().unwrap() {
        d[k] = v.clone();
    }
    let body = json!({"v": 1, "id": id, "type": "turn_finished", "destination": "gateway",
                      "at": NOW, "data": d});
    (id, serde_json::to_vec(&body).unwrap())
}

#[tokio::test]
async fn the_pinned_turn_finished_vector_is_accepted_and_recorded() {
    let r = rig().await;
    let ts = NOW.to_string();
    assert_eq!(
        sign(
            &Secret::new(KEY),
            &ts,
            TURN_ID,
            "turn_finished",
            TURN_BODY.as_bytes()
        ),
        TURN_SIG
    );
    let mut q = Req::signed(TURN_ID, "turn_finished", TURN_BODY.as_bytes(), NOW);
    q.sig = Some(TURN_SIG.into());
    assert_eq!(send(&r, &q).await, (200, String::new()));
    let t = r.hub.latest_turn("s-1f2e3d").unwrap();
    assert_eq!(
        (t.status, t.last_seq, t.settle),
        (TurnStatus::Completed, 14, 1)
    );
    assert!(r.hub.latest_turn("s-other").is_none());
}

#[tokio::test]
async fn a_late_older_turn_end_does_not_replace_a_newer_one() {
    let r = rig().await;
    let post = |seq, settle, data: Value| {
        let (id, body) = turn(seq, settle, data);
        Req::signed(&id, "turn_finished", &body, NOW)
    };
    let pending = json!({"status": "approval_pending", "approval_id": "apr-1"});
    assert_eq!(send(&r, &post(15, 2, json!({}))).await.0, 200);
    assert_eq!(send(&r, &post(14, 1, pending.clone())).await.0, 200);
    let t = r.hub.latest_turn("s-1f2e3d").unwrap();
    assert_eq!((t.status, t.last_seq), (TurnStatus::Completed, 15));
    // The same position with a higher settle counter is a later settle (a
    // turn that failed before it appended anything).
    let failed = json!({"status": "failed", "code": "provider"});
    assert_eq!(send(&r, &post(15, 3, failed)).await.0, 200);
    let t = r.hub.latest_turn("s-1f2e3d").unwrap();
    assert_eq!(
        (t.status, t.code),
        (TurnStatus::Failed, Some(TurnCode::Provider))
    );
}

#[tokio::test]
async fn a_turn_finished_body_gets_the_tight_cap_whatever_the_config_says() {
    let r = rig().await;
    let (id, body) = turn(14, 1, json!({"padding": "x".repeat(MAX_TURN_BODY)}));
    let (status, text) = send(&r, &Req::signed(&id, "turn_finished", &body, NOW)).await;
    assert_eq!((status, text.as_str()), (413, ""));
    assert!(r.hub.latest_turn("s-1f2e3d").is_none());
}

#[tokio::test]
async fn a_turn_finished_that_disagrees_with_its_id_is_refused() {
    let r = rig().await;
    let (id, body) = turn(14, 1, json!({"last_seq": 15}));
    assert_eq!(
        send(&r, &Req::signed(&id, "turn_finished", &body, NOW))
            .await
            .0,
        400
    );
    let (id, body) = turn(14, 1, json!({"session_id": "s-other"}));
    assert_eq!(
        send(&r, &Req::signed(&id, "turn_finished", &body, NOW))
            .await
            .0,
        400
    );
    let (id, body) = turn(14, 1, json!({"approval_id": "not an id"}));
    assert_eq!(
        send(&r, &Req::signed(&id, "turn_finished", &body, NOW))
            .await
            .0,
        400
    );
    assert!(r.hub.latest_turn("s-1f2e3d").is_none());
    // A status word tole adds later is acknowledged, so tole stops retrying.
    let (id, body) = turn(16, 1, json!({"status": "paused"}));
    assert_eq!(
        send(&r, &Req::signed(&id, "turn_finished", &body, NOW))
            .await
            .0,
        200
    );
    assert_eq!(
        r.hub.latest_turn("s-1f2e3d").unwrap().status,
        TurnStatus::Other
    );
}

// ------------------------------------------------------------- defaults

fn cfg(text: &str) -> Config {
    toml::from_str(text).unwrap()
}

fn env_none(_: &str) -> Option<String> {
    None
}

#[test]
fn the_receiver_is_disabled_by_default_and_binds_loopback() {
    let c = cfg("");
    assert!(!c.notify.enabled);
    c.validate().unwrap();
    // Nothing about it is checked while it is off, not even a bad address.
    cfg("[notify]\nlisten = \"nonsense\"\n").validate().unwrap();
    assert!(c.notify_addr().unwrap().ip().is_loopback());
}

#[test]
fn a_non_loopback_address_needs_an_explicit_opt_in() {
    let off = cfg("[notify]\nenabled = true\nlisten = \"0.0.0.0:7811\"\n");
    let Err(ConfigError::Invalid(p)) = off.validate() else {
        panic!("a public bind must be refused");
    };
    assert!(p[0].contains("loopback"), "{p:?}");
    cfg("[notify]\nenabled = true\nlisten = \"0.0.0.0:7811\"\nallow_non_loopback = true\n")
        .validate()
        .unwrap();
    cfg("[notify]\nenabled = true\nlisten = \"[::1]:7811\"\n")
        .validate()
        .unwrap();
    for bad in [
        "max_body_bytes = 10",
        "max_body_bytes = 99999999",
        "path = \"x\"",
        "destination = \"Bad Name\"",
        "fallback_poll_secs = 0",
    ] {
        let c = cfg(&format!("[notify]\nenabled = true\n{bad}\n"));
        assert!(c.validate().is_err(), "{bad}");
    }
}

#[test]
fn the_key_comes_from_the_environment_or_a_private_file_never_the_config() {
    assert!(toml::from_str::<Config>("[notify]\nhmac_key = \"inline-key-0123456789\"\n").is_err());
    let c = cfg("[notify]\nenabled = true\n");
    assert!(matches!(
        c.webhook_key(&env_none),
        Err(ConfigError::Missing(_))
    ));
    let env = |k: &str| (k == ENV_WEBHOOK_KEY).then(|| "env-key-0123456789abcdef".to_owned());
    assert_eq!(
        c.webhook_key(&env).unwrap(),
        Secret::new("env-key-0123456789abcdef")
    );
    let short = |k: &str| (k == ENV_WEBHOOK_KEY).then(|| "short".to_owned());
    let e = c.webhook_key(&short).unwrap_err().to_string();
    assert!(!e.contains("short\n") && e.contains("at least"), "{e}");

    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("key");
    std::fs::write(&f, "file-key-0123456789abcdef\n").unwrap();
    std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o644)).unwrap();
    let c = cfg(&format!(
        "[notify]\nenabled = true\nhmac_key_file = \"{}\"\n",
        f.display()
    ));
    assert!(matches!(
        c.webhook_key(&env_none),
        Err(ConfigError::InsecurePermissions { .. })
    ));
    std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        c.webhook_key(&env_none).unwrap(),
        Secret::new("file-key-0123456789abcdef")
    );
}

#[test]
fn the_receiver_config_never_prints_the_key() {
    let c = ReceiverConfig {
        key: Secret::new(KEY),
        path: PATH.into(),
        destination: "gateway".into(),
        max_body_bytes: 1,
    };
    assert!(!format!("{c:?}").contains(KEY));
}
