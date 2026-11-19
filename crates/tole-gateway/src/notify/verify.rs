//! Verification of one tole v2 delivery. Pure: the clock is an argument.
//!
//! tole signs `HMAC-SHA256(key, "v2\n<ts>\n<id>\n<event>\n<body>")` and sends
//! it as `X-Tole-Signature: v2=<hex>` (tole `docs/webhook-delivery-design.md`
//! section 3). Nothing a delivery carries is trusted before [`verify`] passes.

use ring::hmac;

use crate::secret::Secret;

/// A delivery's timestamp may differ from the receiver's clock by this much.
pub const WINDOW_SECS: u64 = 300;

/// The four `X-Tole-*` header values of one delivery, as received.
#[derive(Debug, Clone, Copy)]
pub struct Headers<'a> {
    /// `X-Tole-Timestamp`.
    pub timestamp: &'a str,
    /// `X-Tole-Delivery`.
    pub id: &'a str,
    /// `X-Tole-Event`.
    pub event: &'a str,
    /// `X-Tole-Signature`.
    pub signature: &'a str,
}

/// Why a delivery was refused. Internal only: the HTTP answer never says which
/// check failed, and no variant carries a received value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reject {
    /// A header is missing or not well formed.
    Malformed,
    /// The signature does not match.
    BadSignature,
    /// The timestamp is outside the window.
    Stale,
}

/// `[A-Za-z0-9._:-]`, at most `max` bytes, not empty (tole's delivery id rule).
pub fn valid_token(s: &str, max: usize) -> bool {
    !s.is_empty()
        && s.len() <= max
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
}

fn input(ts: &str, id: &str, event: &str, body: &[u8]) -> Vec<u8> {
    let mut m = Vec::with_capacity(body.len() + 64);
    m.extend_from_slice(b"v2\n");
    m.extend_from_slice(ts.as_bytes());
    m.push(b'\n');
    m.extend_from_slice(id.as_bytes());
    m.push(b'\n');
    m.extend_from_slice(event.as_bytes());
    m.push(b'\n');
    m.extend_from_slice(body);
    m
}

/// The `v2=<hex>` header value for a delivery. The gateway only receives; this
/// exists so tests can play tole.
pub fn sign(key: &Secret, ts: &str, id: &str, event: &str, body: &[u8]) -> String {
    let k = hmac::Key::new(hmac::HMAC_SHA256, key.expose().as_bytes());
    let tag = hmac::sign(&k, &input(ts, id, event, body));
    let mut out = String::from("v2=");
    for b in tag.as_ref() {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    s.as_bytes()
        .chunks(2)
        .map(|c| {
            let hi = (c[0] as char).to_digit(16)?;
            let lo = (c[1] as char).to_digit(16)?;
            Some((hi * 16 + lo) as u8)
        })
        .collect()
}

/// Check the headers' shape, the signature over the raw `body` (constant-time
/// compare) and the timestamp window against `now_secs`.
pub fn verify(key: &Secret, h: &Headers<'_>, body: &[u8], now_secs: u64) -> Result<(), Reject> {
    let ts_ok = !h.timestamp.is_empty()
        && h.timestamp.len() <= 12
        && h.timestamp.bytes().all(|b| b.is_ascii_digit());
    if !ts_ok || !valid_token(h.id, 128) || !valid_token(h.event, 64) {
        return Err(Reject::Malformed);
    }
    let tag = h
        .signature
        .strip_prefix("v2=")
        .and_then(unhex)
        .filter(|t| t.len() == 32)
        .ok_or(Reject::Malformed)?;
    let k = hmac::Key::new(hmac::HMAC_SHA256, key.expose().as_bytes());
    // `ring::hmac::verify` compares in constant time.
    hmac::verify(&k, &input(h.timestamp, h.id, h.event, body), &tag)
        .map_err(|_| Reject::BadSignature)?;
    let ts: u64 = h.timestamp.parse().map_err(|_| Reject::Malformed)?;
    if now_secs.abs_diff(ts) > WINDOW_SECS {
        return Err(Reject::Stale);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // The vector pinned in tole's docs/webhook-delivery-design.md section 3.
    const KEY: &str = "tole-test-key-0123456789abcdef";
    const TS: &str = "1795000000";
    const ID: &str = "cron:daily-1:0123456789abcdef";
    const EVENT: &str = "cron_run_finished";
    const BODY: &str = r#"{"v":1,"id":"cron:daily-1:0123456789abcdef","type":"cron_run_finished","destination":"gateway","at":1795000000,"data":{"job":"daily","status":"ok","summary":"all good","truncated":false}}"#;
    const SIG: &str = "v2=9d8c931c19daf1192cfb7509c7f10dd86faf8ff0c938ae22d9ca7d99d8764f62";

    fn headers<'a>(sig: &'a str) -> Headers<'a> {
        Headers {
            timestamp: TS,
            id: ID,
            event: EVENT,
            signature: sig,
        }
    }

    #[test]
    fn signing_matches_the_pinned_tole_vector() {
        let key = Secret::new(KEY);
        assert_eq!(sign(&key, TS, ID, EVENT, BODY.as_bytes()), SIG);
    }

    #[test]
    fn the_pinned_vector_verifies() {
        let key = Secret::new(KEY);
        assert_eq!(
            verify(&key, &headers(SIG), BODY.as_bytes(), 1795000000),
            Ok(())
        );
    }

    #[test]
    fn any_changed_input_fails() {
        let key = Secret::new(KEY);
        let now = 1795000000;
        let bad_body = BODY.replace("all good", "all gold");
        assert_eq!(
            verify(&key, &headers(SIG), bad_body.as_bytes(), now),
            Err(Reject::BadSignature)
        );
        let mut h = headers(SIG);
        h.id = "cron:daily-2:0123456789abcdef";
        assert_eq!(
            verify(&key, &h, BODY.as_bytes(), now),
            Err(Reject::BadSignature)
        );
        let mut h = headers(SIG);
        h.event = "approval_requested";
        assert_eq!(
            verify(&key, &h, BODY.as_bytes(), now),
            Err(Reject::BadSignature)
        );
        assert_eq!(
            verify(
                &Secret::new("another-key-0123456789"),
                &headers(SIG),
                BODY.as_bytes(),
                now
            ),
            Err(Reject::BadSignature)
        );
    }

    #[test]
    fn malformed_signatures_are_refused_before_any_compare() {
        let key = Secret::new(KEY);
        for sig in ["", "v1=00", "v2=", "v2=zz", &SIG[..SIG.len() - 2], "9d8c"] {
            assert_eq!(
                verify(&key, &headers(sig), BODY.as_bytes(), 1795000000),
                Err(Reject::Malformed),
                "{sig}"
            );
        }
    }

    #[test]
    fn the_window_is_300_seconds_either_way() {
        let key = Secret::new(KEY);
        let at = |now| verify(&key, &headers(SIG), BODY.as_bytes(), now);
        assert_eq!(at(1795000300), Ok(()));
        assert_eq!(at(1794999700), Ok(()));
        assert_eq!(at(1795000301), Err(Reject::Stale));
        assert_eq!(at(1794999699), Err(Reject::Stale));
    }

    #[test]
    fn odd_timestamps_and_ids_are_malformed() {
        let key = Secret::new(KEY);
        for ts in ["", "-1", "1e9", "17950000000000", "１２３"] {
            let mut h = headers(SIG);
            h.timestamp = ts;
            assert_eq!(verify(&key, &h, b"", 0), Err(Reject::Malformed), "{ts}");
        }
        let long = "a".repeat(129);
        for id in ["", "a b", "a\nb", long.as_str()] {
            let mut h = headers(SIG);
            h.id = id;
            assert_eq!(verify(&key, &h, b"", 0), Err(Reject::Malformed));
        }
    }
}
