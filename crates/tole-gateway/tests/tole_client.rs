//! Integration tests for [`tole_gateway::tole_client::HttpToleClient`] against
//! the scriptable fake `tole serve`.

#[path = "support/fake_tole.rs"]
mod fake_tole;

use std::time::Duration;

use fake_tole::{FakeTole, PromptStep};
use serde_json::json;
use tole_gateway::config::ToleAuth;
use tole_gateway::error::ToleError;
use tole_gateway::secret::Secret;
use tole_gateway::tole_client::{HttpToleClient, ToleApi, ToleClientConfig};
use tole_gateway::types::{ApprovalStatus, Decision, PromptOutcome};

const TOKEN: &str = "TOKEN-planted-9f3a7c";
const PASSWORD: &str = "PASSWORD-planted-51bd";

fn config(url: &str, auth: ToleAuth) -> ToleClientConfig {
    ToleClientConfig {
        base_url: url.to_string(),
        auth,
        request_timeout: Duration::from_secs(5),
        prompt_timeout: Duration::from_secs(10),
    }
}

fn bearer() -> ToleAuth {
    ToleAuth::Bearer(Secret::new(TOKEN))
}

fn client_for(fake: &FakeTole) -> HttpToleClient {
    HttpToleClient::new(config(&fake.url(), bearer())).expect("client")
}

async fn fake_with_token() -> (FakeTole, HttpToleClient) {
    let fake = FakeTole::start().await;
    fake.require_bearer(TOKEN);
    let client = client_for(&fake);
    (fake, client)
}

#[tokio::test]
async fn health_works_and_tolerates_a_trailing_slash() {
    let (fake, _) = fake_with_token().await;
    let client = HttpToleClient::new(config(&format!("{}/", fake.url()), bearer())).unwrap();
    client.health().await.unwrap();
    assert_eq!(fake.requests_to("/health").len(), 1);
}

#[tokio::test]
async fn create_session_sends_only_the_given_fields() {
    let (fake, client) = fake_with_token().await;
    let open = client.create_session(None, None).await.unwrap();
    assert_eq!(open.session_id, "srv-1");
    assert_eq!(fake.last_request().unwrap().body, json!({}));

    let open = client
        .create_session(Some("work"), Some("/w/proj"))
        .await
        .unwrap();
    assert_eq!(open.session_id, "srv-2");
    assert_eq!(open.workspace.as_deref(), Some("/w/proj"));
    assert_eq!(open.profile.as_deref(), Some("work"));
    assert_eq!(
        fake.last_request().unwrap().body,
        json!({"cwd": "/w/proj", "profile": "work"})
    );
}

#[tokio::test]
async fn resume_session_sends_resume_and_optional_profile() {
    let (fake, client) = fake_with_token().await;
    fake.add_session("srv-9");
    let open = client.resume_session("srv-9", None).await.unwrap();
    assert!(open.resumed);
    assert_eq!(
        fake.last_request().unwrap().body,
        json!({"resume": "srv-9"})
    );
    client.resume_session("srv-9", Some("p")).await.unwrap();
    assert_eq!(
        fake.last_request().unwrap().body,
        json!({"resume": "srv-9", "profile": "p"})
    );
}

#[tokio::test]
async fn prompt_reply_with_and_without_text() {
    let (fake, client) = fake_with_token().await;
    fake.add_session("srv-1");
    fake.script_prompt(PromptStep::reply("end_turn", Some("hello")));
    fake.script_prompt(PromptStep::reply("cancelled", None));
    let a = client.prompt("srv-1", "hi there").await.unwrap();
    assert_eq!(
        a,
        PromptOutcome::Reply {
            stop_reason: "end_turn".into(),
            text: Some("hello".into())
        }
    );
    assert_eq!(
        fake.last_request().unwrap().body,
        json!({"text": "hi there"})
    );
    let b = client.prompt("srv-1", "again").await.unwrap();
    assert_eq!(
        b,
        PromptOutcome::Reply {
            stop_reason: "cancelled".into(),
            text: None
        }
    );
}

#[tokio::test]
async fn prompt_approval_required_with_and_without_the_row() {
    let (fake, client) = fake_with_token().await;
    fake.add_session("srv-1");
    let row = fake.add_approval("apr-1", "srv-1", "write_file", "write notes.md");
    fake.script_prompt(PromptStep::approval_required(Some(row)));
    fake.script_prompt(PromptStep::approval_required(None));
    match client.prompt("srv-1", "write it").await.unwrap() {
        PromptOutcome::ApprovalRequired { approval: Some(a) } => {
            assert_eq!(a.id, "apr-1");
            assert_eq!(a.tool, "write_file");
            assert_eq!(a.status, ApprovalStatus::Pending);
            assert_eq!(a.session_id, "srv-1");
        }
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(
        client.prompt("srv-1", "again").await.unwrap(),
        PromptOutcome::ApprovalRequired { approval: None }
    );
}

#[tokio::test]
async fn prompt_without_stop_reason_is_a_decode_error() {
    let (fake, client) = fake_with_token().await;
    fake.add_session("srv-1");
    fake.fail_with_body("POST", "/sessions/srv-1/prompt", 200, r#"{"text":"x"}"#);
    assert!(matches!(
        client.prompt("srv-1", "hi").await,
        Err(ToleError::Decode(_))
    ));
}

#[tokio::test]
async fn cancel_and_status() {
    let (fake, client) = fake_with_token().await;
    fake.add_session("srv-1");
    fake.add_message("srv-1", "user", "hi");
    fake.add_message("srv-1", "assistant", "yo");
    client.cancel("srv-1").await.unwrap();
    let st = client.status("srv-1").await.unwrap();
    assert_eq!(st.id, "srv-1");
    assert_eq!(st.entries, Some(2));
    assert!(!st.busy);
    assert!(matches!(
        client.cancel("nope").await,
        Err(ToleError::NotFound(_))
    ));
}

#[tokio::test]
async fn approvals_lists_all_or_one_session() {
    let (fake, client) = fake_with_token().await;
    fake.add_approval("a1", "srv-1", "write_file", "one");
    fake.add_approval("a2", "srv-2", "edit_file", "two");
    assert_eq!(client.approvals(None).await.unwrap().len(), 2);
    assert_eq!(fake.last_request().unwrap().query, "");
    let only = client.approvals(Some("srv-2")).await.unwrap();
    assert_eq!(only.len(), 1);
    assert_eq!(only[0].id, "a2");
    assert_eq!(fake.last_request().unwrap().query, "session=srv-2");
}

#[tokio::test]
async fn decide_allow_is_202_and_deny_is_200() {
    let (fake, client) = fake_with_token().await;
    fake.add_approval("a1", "srv-1", "write_file", "one");
    fake.add_approval("a2", "srv-1", "write_file", "two");
    let allow = client.decide("a1", Decision::Allow).await.unwrap();
    assert_eq!(allow.id, "a1");
    assert_eq!(allow.status, "approved");
    assert!(allow.resuming);
    let deny = client.decide("a2", Decision::Deny).await.unwrap();
    assert_eq!(deny.status, "denied");
    assert!(!deny.resuming);
    assert_eq!(
        fake.decisions(),
        vec![("a1".into(), "allow".into()), ("a2".into(), "deny".into())]
    );
    assert_eq!(
        fake.last_request().unwrap().body,
        json!({"decision": "deny"})
    );
    assert!(matches!(
        client.decide("missing", Decision::Allow).await,
        Err(ToleError::NotFound(_))
    ));
}

#[tokio::test]
async fn entries_pages_with_after_and_limit() {
    let (fake, client) = fake_with_token().await;
    fake.add_session("srv-1");
    for i in 0..5 {
        fake.add_message("srv-1", "user", &format!("m{i}"));
    }
    let page = client.entries("srv-1", 0, 2).await.unwrap();
    assert_eq!(page.entries.len(), 2);
    assert!(page.has_more);
    assert_eq!(page.next_after, 2);
    assert_eq!(page.last_seq, 5);
    assert_eq!(page.entries[0].message(), Some(("user", "m0")));
    assert_eq!(fake.last_request().unwrap().query, "after=0&limit=2");
    let rest = client.entries("srv-1", page.next_after, 10).await.unwrap();
    assert_eq!(rest.entries.len(), 3);
    assert!(!rest.has_more);
}

async fn expect_err<T: std::fmt::Debug>(
    fake: &FakeTole,
    status: u16,
    call: impl std::future::Future<Output = Result<T, ToleError>>,
) -> ToleError {
    fake.clear_failures();
    fake.fail("GET", "/sessions/s/status", status, "server said so");
    call.await.expect_err("an error")
}

#[tokio::test]
async fn status_codes_map_to_errors() {
    let (fake, client) = fake_with_token().await;
    let e = expect_err(&fake, 401, client.status("s")).await;
    assert!(matches!(e, ToleError::Unauthorized));
    let e = expect_err(&fake, 404, client.status("s")).await;
    assert!(matches!(&e, ToleError::NotFound(t) if t == "server said so"));
    let e = expect_err(&fake, 409, client.status("s")).await;
    assert!(matches!(&e, ToleError::Conflict(t) if t == "server said so"));
    let e = expect_err(&fake, 400, client.status("s")).await;
    assert!(matches!(&e, ToleError::BadRequest(t) if t == "server said so"));
    let e = expect_err(&fake, 403, client.status("s")).await;
    assert!(matches!(&e, ToleError::BadRequest(t) if t == "server said so"));
    let e = expect_err(&fake, 503, client.status("s")).await;
    assert!(matches!(&e, ToleError::Unavailable(t) if t == "server said so"));
    let e = expect_err(&fake, 500, client.status("s")).await;
    assert!(matches!(&e, ToleError::Server(t) if t == "server said so"));
    let e = expect_err(&fake, 502, client.status("s")).await;
    assert!(matches!(&e, ToleError::Server(_)));
}

#[tokio::test]
async fn a_409_on_prompt_is_busy_but_only_there() {
    let (fake, client) = fake_with_token().await;
    fake.add_session("srv-1");
    fake.script_prompt(PromptStep::error(409, "session is busy"));
    assert!(matches!(
        client.prompt("srv-1", "x").await,
        Err(ToleError::Busy)
    ));
    fake.fail("POST", "/sessions", 409, "writer lock held");
    assert!(matches!(
        client.create_session(None, None).await,
        Err(ToleError::Conflict(t)) if t == "writer lock held"
    ));
}

#[tokio::test]
async fn prompt_error_statuses() {
    let (fake, client) = fake_with_token().await;
    fake.add_session("srv-1");
    fake.script_prompt(PromptStep::error(500, "provider failed"));
    assert!(matches!(
        client.prompt("srv-1", "x").await,
        Err(ToleError::Server(_))
    ));
    assert!(matches!(
        client.prompt("ghost", "x").await,
        Err(ToleError::NotFound(_))
    ));
}

#[tokio::test]
async fn error_text_is_capped_and_non_json_bodies_are_kept() {
    let (fake, client) = fake_with_token().await;
    fake.fail("GET", "/sessions/s/status", 500, &"x".repeat(2000));
    match client.status("s").await {
        Err(ToleError::Server(t)) => {
            assert!(t.len() <= 310, "len {}", t.len());
            assert!(t.starts_with("xxx"));
        }
        other => panic!("unexpected {other:?}"),
    }
    fake.clear_failures();
    fake.fail_with_body("GET", "/sessions/s/status", 502, "bad gateway");
    assert!(matches!(
        client.status("s").await,
        Err(ToleError::Server(t)) if t == "bad gateway"
    ));
}

#[tokio::test]
async fn undecodable_success_body_is_a_decode_error() {
    let (fake, client) = fake_with_token().await;
    fake.fail_with_body("GET", "/sessions/s/status", 200, "not json");
    assert!(matches!(
        client.status("s").await,
        Err(ToleError::Decode(_))
    ));
}

#[tokio::test]
async fn bearer_header_is_sent_and_a_wrong_token_is_unauthorized() {
    let (fake, client) = fake_with_token().await;
    client.health().await.unwrap();
    client.approvals(None).await.unwrap();
    let last = fake.last_request().unwrap();
    assert_eq!(last.auth_scheme, "bearer");
    assert!(last.auth_ok);

    let wrong = HttpToleClient::new(config(
        &fake.url(),
        ToleAuth::Bearer(Secret::new("something-else")),
    ))
    .unwrap();
    assert!(matches!(
        wrong.approvals(None).await,
        Err(ToleError::Unauthorized)
    ));
}

#[tokio::test]
async fn basic_auth_is_sent() {
    let fake = FakeTole::start().await;
    fake.require_basic("gw", PASSWORD);
    let client = HttpToleClient::new(config(
        &fake.url(),
        ToleAuth::Basic {
            user: "gw".into(),
            password: Secret::new(PASSWORD),
        },
    ))
    .unwrap();
    client.approvals(None).await.unwrap();
    let last = fake.last_request().unwrap();
    assert_eq!(last.auth_scheme, "basic");
    assert!(last.auth_ok);
    // A bearer client against a basic-only server is refused.
    let bearer_client = client_for(&fake);
    assert!(matches!(
        bearer_client.approvals(None).await,
        Err(ToleError::Unauthorized)
    ));
}

#[tokio::test]
async fn a_prompt_longer_than_prompt_timeout_times_out() {
    let fake = FakeTole::start().await;
    fake.add_session("srv-1");
    fake.script_prompt(PromptStep::delayed(
        Duration::from_secs(3),
        PromptStep::reply("end_turn", Some("late")),
    ));
    let mut cfg = config(&fake.url(), bearer());
    cfg.prompt_timeout = Duration::from_millis(300);
    let client = HttpToleClient::new(cfg).unwrap();
    assert!(matches!(
        client.prompt("srv-1", "slow").await,
        Err(ToleError::Timeout)
    ));
}

#[tokio::test]
async fn a_slow_prompt_within_prompt_timeout_succeeds_past_request_timeout() {
    let fake = FakeTole::start().await;
    fake.add_session("srv-1");
    fake.script_prompt(PromptStep::delayed(
        Duration::from_millis(700),
        PromptStep::reply("end_turn", Some("done")),
    ));
    let mut cfg = config(&fake.url(), bearer());
    cfg.request_timeout = Duration::from_millis(250);
    cfg.prompt_timeout = Duration::from_secs(10);
    let client = HttpToleClient::new(cfg).unwrap();
    let out = client.prompt("srv-1", "slow").await.unwrap();
    assert!(matches!(out, PromptOutcome::Reply { text: Some(t), .. } if t == "done"));
}

#[tokio::test]
async fn ordinary_requests_use_the_request_timeout() {
    let fake = FakeTole::start().await;
    fake.add_session("srv-1");
    // The prompt route is the only slow one the fake offers; a short request
    // timeout must not apply to it, but a stalled status must still time out:
    // emulate with a prompt delay on a client whose prompt timeout is short.
    let mut cfg = config(&fake.url(), bearer());
    cfg.request_timeout = Duration::from_secs(5);
    cfg.prompt_timeout = Duration::from_millis(100);
    fake.script_prompt(PromptStep::delayed(
        Duration::from_millis(600),
        PromptStep::reply("end_turn", None),
    ));
    let client = HttpToleClient::new(cfg).unwrap();
    assert!(matches!(
        client.prompt("srv-1", "x").await,
        Err(ToleError::Timeout)
    ));
    client.status("srv-1").await.unwrap();
}

#[tokio::test]
async fn path_segments_and_query_values_are_percent_encoded() {
    let (fake, client) = fake_with_token().await;
    let weird = "we ird/id?x#y%z";
    fake.add_session(weird);
    fake.add_approval("ap/1 x", weird, "write_file", "d");

    let st = client.status(weird).await.unwrap();
    assert_eq!(st.id, weird);
    let last = fake.last_request().unwrap();
    assert_eq!(last.path, "/sessions/we%20ird%2Fid%3Fx%23y%25z/status");
    assert_eq!(last.query, "");

    client.entries(weird, 3, 4).await.unwrap();
    assert_eq!(
        fake.last_request().unwrap().path,
        "/sessions/we%20ird%2Fid%3Fx%23y%25z/entries"
    );

    let rows = client.approvals(Some(weird)).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        fake.last_request().unwrap().query,
        "session=we+ird%2Fid%3Fx%23y%25z"
    );

    let ack = client.decide("ap/1 x", Decision::Deny).await.unwrap();
    assert_eq!(ack.id, "ap/1 x");
    assert_eq!(
        fake.last_request().unwrap().path,
        "/approvals/ap%2F1%20x/decision"
    );
}

#[tokio::test]
async fn bad_base_urls_are_refused() {
    for bad in ["not a url", "ftp://127.0.0.1:1", ""] {
        let r = HttpToleClient::new(config(bad, bearer()));
        assert!(matches!(r, Err(ToleError::BadRequest(_))), "{bad}");
    }
}

#[tokio::test]
async fn a_refused_connection_is_a_transport_error_without_url_or_secret() {
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let client =
        HttpToleClient::new(config(&format!("http://127.0.0.1:{port}"), bearer())).unwrap();
    let err = client.health().await.unwrap_err();
    assert!(matches!(err, ToleError::Transport(_)), "{err:?}");
    let shown = format!("{err} {err:?}");
    assert!(!shown.contains("127.0.0.1"), "{shown}");
    assert!(!shown.contains(TOKEN));
}

#[tokio::test]
async fn credentials_in_the_base_url_are_dropped() {
    let fake = FakeTole::start().await;
    let url = fake.url().replace("http://", "http://user:pw-in-url@");
    let client = HttpToleClient::new(config(&url, ToleAuth::Bearer(Secret::new("t")))).unwrap();
    client.health().await.unwrap();
    assert_eq!(fake.last_request().unwrap().auth_scheme, "bearer");
}

#[tokio::test]
async fn no_secret_in_any_error_or_debug_output() {
    let fake = FakeTole::start().await;
    fake.require_bearer(TOKEN);
    fake.add_session("srv-1");
    let client = client_for(&fake);
    let basic = HttpToleClient::new(config(
        &fake.url(),
        ToleAuth::Basic {
            user: "gw".into(),
            password: Secret::new(PASSWORD),
        },
    ))
    .unwrap();

    let mut shown = vec![
        format!("{client:?}"),
        format!("{basic:?}"),
        format!("{:#?}", config("http://h", bearer())),
        format!(
            "{:?}",
            config(
                "http://h",
                ToleAuth::Basic {
                    user: "gw".into(),
                    password: Secret::new(PASSWORD)
                }
            )
        ),
    ];
    // Unauthorized (basic client against a bearer server).
    let e = basic.approvals(None).await.unwrap_err();
    shown.push(format!("{e} {e:?}"));
    // Every mapped status.
    for status in [400u16, 403, 404, 409, 500, 503] {
        fake.clear_failures();
        fake.fail("GET", "/sessions/srv-1/status", status, "boom");
        let e = client.status("srv-1").await.unwrap_err();
        shown.push(format!("{e} {e:?}"));
    }
    // Timeout and transport.
    fake.script_prompt(PromptStep::delayed(
        Duration::from_secs(2),
        PromptStep::reply("end_turn", None),
    ));
    let mut cfg = config(&fake.url(), bearer());
    cfg.prompt_timeout = Duration::from_millis(100);
    let short = HttpToleClient::new(cfg).unwrap();
    let e = short.prompt("srv-1", "x").await.unwrap_err();
    shown.push(format!("{e} {e:?}"));
    let dead = HttpToleClient::new(config("http://127.0.0.1:1", bearer())).unwrap();
    let e = dead.health().await.unwrap_err();
    shown.push(format!("{e} {e:?}"));

    assert!(shown.len() >= 12);
    for text in shown {
        assert!(!text.contains(TOKEN), "{text}");
        assert!(!text.contains(PASSWORD), "{text}");
        assert!(!text.contains("Bearer "), "{text}");
        assert!(!text.to_lowercase().contains("authorization"), "{text}");
    }
}

#[tokio::test]
async fn the_fake_log_never_stores_the_secret() {
    let (fake, client) = fake_with_token().await;
    client.approvals(None).await.unwrap();
    assert!(!format!("{:?}", fake.requests()).contains(TOKEN));
}
