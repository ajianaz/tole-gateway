//! The Bot API HTTP client and the error mapping.
//!
//! The bot token lives in the request path, so every error that leaves this
//! module is built from fixed strings or scrubbed text: never from a `reqwest`
//! error that still carries its URL, and never from text that contains the
//! token.

use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

use crate::error::ChatError;
use crate::secret::Secret;

/// Extra time the HTTP request may take beyond the long-poll timeout.
const POLL_MARGIN: Duration = Duration::from_secs(15);
/// Timeout for every non-polling request.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Deserialize)]
struct Envelope {
    ok: bool,
    result: Option<Value>,
    description: Option<String>,
    parameters: Option<Parameters>,
}

#[derive(Deserialize)]
struct Parameters {
    retry_after: Option<u64>,
}

/// A thin Bot API client. Not `Debug` on purpose.
#[derive(Clone)]
pub(crate) struct Api {
    client: reqwest::Client,
    /// `{api_base}/bot<token>`; secret, never printed.
    prefix: String,
    token: Secret,
    poll_timeout: Duration,
}

/// True when the base URL points at this machine; such calls must bypass any
/// proxy configured in the environment.
pub(crate) fn is_loopback(api_base: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(api_base) else {
        return false;
    };
    let Some(host) = url.host_str() else {
        return false;
    };
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<std::net::IpAddr>()
        .is_ok_and(|ip| ip.is_loopback())
}

impl Api {
    pub(crate) fn new(
        api_base: &str,
        token: Secret,
        poll_timeout: Duration,
    ) -> Result<Self, ChatError> {
        let mut builder = reqwest::Client::builder().timeout(REQUEST_TIMEOUT);
        if is_loopback(api_base) {
            builder = builder.no_proxy();
        }
        let client = builder
            .build()
            .map_err(|e| ChatError::Transport(scrub_reqwest(&e, &token)))?;
        let prefix = format!("{}/bot{}", api_base.trim_end_matches('/'), token.expose());
        Ok(Self {
            client,
            prefix,
            token,
            poll_timeout,
        })
    }

    /// Call one method and return its `result`.
    pub(crate) async fn call(
        &self,
        method: &str,
        body: &Value,
        timeout: Option<Duration>,
    ) -> Result<Value, ChatError> {
        let url = format!("{}/{}", self.prefix, method);
        let mut req = self.client.post(url).json(body);
        if let Some(t) = timeout {
            req = req.timeout(t);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| ChatError::Transport(scrub_reqwest(&e, &self.token)))?;
        let status = resp.status().as_u16();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| ChatError::Transport(scrub_reqwest(&e, &self.token)))?;
        classify(status, &bytes, self.token.expose())
    }

    /// `getUpdates` with the long-poll timeout.
    pub(crate) async fn get_updates(&self, offset: Option<i64>) -> Result<Vec<Value>, ChatError> {
        let mut body = serde_json::json!({
            "timeout": self.poll_timeout.as_secs(),
            "allowed_updates": ["message", "callback_query"],
        });
        if let Some(o) = offset {
            body["offset"] = Value::from(o);
        }
        let result = self
            .call("getUpdates", &body, Some(self.poll_timeout + POLL_MARGIN))
            .await?;
        match result {
            Value::Array(items) => Ok(items),
            _ => Err(ChatError::Decode(
                "getUpdates result is not an array".into(),
            )),
        }
    }
}

/// Turn a transport failure into a fixed message: the `reqwest` text can carry
/// the URL (and so the token), so it is never copied.
fn scrub_reqwest(e: &reqwest::Error, _token: &Secret) -> String {
    let kind = if e.is_timeout() {
        "request timed out"
    } else if e.is_connect() {
        "connection failed"
    } else if e.is_decode() || e.is_body() {
        "reading the response failed"
    } else {
        "request failed"
    };
    kind.to_string()
}

/// Replace every occurrence of the token.
fn scrub(text: &str, token: &str) -> String {
    if token.is_empty() {
        text.to_string()
    } else {
        text.replace(token, "***")
    }
}

/// Map an HTTP status and body to the result or a [`ChatError`].
pub(crate) fn classify(status: u16, body: &[u8], token: &str) -> Result<Value, ChatError> {
    let envelope: Option<Envelope> = serde_json::from_slice(body).ok();
    let retry_after = envelope
        .as_ref()
        .and_then(|e| e.parameters.as_ref())
        .and_then(|p| p.retry_after);
    if status == 429 || retry_after.is_some() {
        return Err(ChatError::RateLimited {
            retry_after_secs: retry_after.unwrap_or(1),
        });
    }
    let description = envelope
        .as_ref()
        .and_then(|e| e.description.clone())
        .unwrap_or_default();
    let lower = description.to_ascii_lowercase();
    if status == 403
        || lower.contains("bot was blocked")
        || lower.contains("chat not found")
        || lower.contains("user is deactivated")
    {
        return Err(ChatError::Gone);
    }
    match envelope {
        Some(Envelope {
            ok: true,
            result: Some(result),
            ..
        }) if (200..300).contains(&status) => Ok(result),
        Some(Envelope { ok: true, .. }) => Err(ChatError::Decode("missing result".into())),
        Some(_) => {
            let text = if description.is_empty() {
                format!("HTTP {status}")
            } else {
                scrub(&description, token)
            };
            Err(ChatError::Api(text))
        }
        None if (200..300).contains(&status) => {
            Err(ChatError::Decode("response is not valid JSON".into()))
        }
        None => Err(ChatError::Api(format!("HTTP {status}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_detection() {
        assert!(is_loopback("http://127.0.0.1:1234"));
        assert!(is_loopback("http://localhost:80"));
        assert!(is_loopback("http://[::1]:8080"));
        assert!(!is_loopback("https://api.telegram.org"));
        assert!(!is_loopback("not a url"));
    }

    #[test]
    fn classify_ok_and_errors() {
        let ok = br#"{"ok":true,"result":[1]}"#;
        assert_eq!(classify(200, ok, "t").unwrap(), serde_json::json!([1]));
        let rl = br#"{"ok":false,"error_code":429,"description":"Too Many Requests","parameters":{"retry_after":7}}"#;
        assert!(matches!(
            classify(429, rl, "t"),
            Err(ChatError::RateLimited {
                retry_after_secs: 7
            })
        ));
        // retry_after on a non-429 status still counts.
        assert!(matches!(
            classify(400, rl, "t"),
            Err(ChatError::RateLimited { .. })
        ));
        assert!(matches!(classify(403, b"{}", "t"), Err(ChatError::Gone)));
        let blocked = br#"{"ok":false,"description":"Forbidden: bot was blocked by the user"}"#;
        assert!(matches!(classify(200, blocked, "t"), Err(ChatError::Gone)));
        let nf = br#"{"ok":false,"description":"Bad Request: chat not found"}"#;
        assert!(matches!(classify(400, nf, "t"), Err(ChatError::Gone)));
        let bad = br#"{"ok":false,"description":"Bad Request: nope"}"#;
        assert!(matches!(classify(400, bad, "t"), Err(ChatError::Api(m)) if m.contains("nope")));
        assert!(matches!(
            classify(200, b"<html>", "t"),
            Err(ChatError::Decode(_))
        ));
        assert!(matches!(
            classify(502, b"<html>", "t"),
            Err(ChatError::Api(_))
        ));
    }

    #[test]
    fn description_is_scrubbed() {
        let body = br#"{"ok":false,"description":"bad 123:SECRET here"}"#;
        let err = classify(400, body, "123:SECRET").unwrap_err();
        assert!(!err.to_string().contains("SECRET"));
    }
}
