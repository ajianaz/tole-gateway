//! The leak check, alone in its binary: it installs a tracing subscriber, and
//! another test emitting events concurrently can make the capture flaky.

use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use tole_gateway::notify::{sign, NotifyHub, Receiver, ReceiverConfig};
use tole_gateway::secret::Secret;

const KEY: &str = "tole-test-key-0123456789abcdef";
const NOW: u64 = 1_795_000_000;
const PATH: &str = "/tole/events";

struct Rig {
    url: String,
    http: reqwest::Client,
}

async fn rig() -> Rig {
    let hub = Arc::new(NotifyHub::new(Duration::from_secs(15)));
    let clock = Arc::new(AtomicU64::new(NOW));
    let receiver = Receiver::with_clock(
        ReceiverConfig {
            key: Secret::new(KEY),
            path: PATH.into(),
            destination: "gateway".into(),
            max_body_bytes: 64 * 1024,
        },
        hub,
        Arc::new(move || clock.load(Ordering::SeqCst)),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(receiver.serve(listener, std::future::pending()));
    Rig {
        url,
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

// ------------------------------------------------------------ leak check

#[derive(Clone, Default)]
struct Log(Arc<Mutex<Vec<u8>>>);

impl Write for Log {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn no_secret_or_signature_reaches_the_logs_or_an_answer() {
    let log = Log::default();
    let sink = log.clone();
    let sub = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || sink.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(sub);

    let r = rig().await;
    let (id, ev, body) = requested("apr-1", "srv-1");
    let good = Req::signed(&id, &ev, &body, NOW);
    let mut bad = Req::signed(&id, &ev, &body, NOW);
    bad.sig = Some(sign(
        &Secret::new("wrong-key-0123456789abcdef"),
        &bad.ts,
        &id,
        &ev,
        &body,
    ));
    let stale = Req::signed(&id, &ev, &body, NOW - 1000);
    let mut missing = Req::signed(&id, &ev, &body, NOW);
    missing.sig = None;

    let mut answers = Vec::new();
    for q in [&bad, &stale, &missing, &good, &good] {
        answers.push(send(&r, q).await);
    }
    let all_text = String::from_utf8(log.0.lock().unwrap().clone()).unwrap();
    assert!(
        all_text.contains("refused") && all_text.contains("duplicate"),
        "the check is only meaningful when the receiver did log: {all_text}"
    );
    let mut needles = vec![KEY.to_owned(), "wrong-key-0123456789abcdef".to_owned()];
    for q in [&bad, &stale, &good] {
        needles.push(q.sig.clone().unwrap());
        needles.push(q.sig.clone().unwrap()[3..].to_owned());
    }
    for n in &needles {
        assert!(
            !all_text.contains(n.as_str()),
            "log leaked a secret or signature: {n}"
        );
        for (_, a) in &answers {
            assert!(!a.contains(n.as_str()));
        }
    }
    assert!(
        !all_text.contains("write notes.md"),
        "event text is not logged"
    );
}
