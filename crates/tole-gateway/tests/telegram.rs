//! Telegram adapter tests against the fake Bot API on loopback.

#[path = "support/fake_telegram.rs"]
mod fake_telegram;

use std::time::{Duration, Instant};

use fake_telegram::FakeTelegram;
use serde_json::json;
use tole_gateway::channel::{
    ApprovalPrompt, Button, ChatId, ChatPort, ChatSource, Inbound, MessageRef, UserId,
};
use tole_gateway::error::ChatError;
use tole_gateway::secret::Secret;
use tole_gateway::telegram::{TelegramAdapter, TelegramConfig};

const TOKEN: &str = "123456:PLANTED-token-ZZZ";

fn config(url: String, poll_secs: u64) -> TelegramConfig {
    TelegramConfig {
        api_base: url,
        bot_token: Secret::new(TOKEN),
        poll_timeout: Duration::from_secs(poll_secs),
    }
}

async fn setup() -> (FakeTelegram, TelegramAdapter) {
    let fake = FakeTelegram::start().await;
    let adapter = TelegramAdapter::new(config(fake.url(), 1)).unwrap();
    (fake, adapter)
}

fn utf16(s: &str) -> usize {
    s.encode_utf16().count()
}

fn sent_texts(fake: &FakeTelegram) -> Vec<String> {
    fake.sent_messages()
        .iter()
        .map(|m| m["text"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn normalizes_message_and_callback() {
    let (fake, tg) = setup().await;
    fake.push_text(42, 42, "hello");
    fake.push_callback(42, 42, "cb1", "a:1", 7);
    let got = tg.next().await.unwrap();
    assert_eq!(
        got,
        vec![
            Inbound::Message {
                chat: ChatId(42),
                user: UserId(42),
                text: "hello".into(),
                message_id: 1
            },
            Inbound::Callback {
                chat: ChatId(42),
                user: UserId(42),
                callback_id: "cb1".into(),
                data: "a:1".into(),
                message: Some(MessageRef {
                    chat: ChatId(42),
                    message_id: 7
                }),
            },
        ]
    );
}

#[tokio::test]
async fn request_shape_and_token_in_path() {
    let (fake, tg) = setup().await;
    fake.push_text(1, 1, "x");
    tg.next().await.unwrap();
    let polls = fake.poll_requests();
    assert_eq!(polls[0]["timeout"], 1);
    assert_eq!(
        polls[0]["allowed_updates"],
        json!(["message", "callback_query"])
    );
    assert!(polls[0].get("offset").is_none());
    assert_eq!(fake.tokens_seen(), vec![format!("bot{TOKEN}")]);
}

#[tokio::test]
async fn ignored_updates_are_skipped_but_acknowledged() {
    let (fake, tg) = setup().await;
    fake.push_group_text(-100, 5, "group");
    fake.push_edited(5, 5, "edited");
    fake.push_sticker(5, 5);
    let last = fake.push_channel_post(-7, "post");
    assert!(tg.next().await.unwrap().is_empty());
    // The next poll must ask past the ignored updates.
    fake.push_text(5, 5, "real");
    let got = tg.next().await.unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(fake.poll_requests()[1]["offset"], last + 1);
    assert_eq!(fake.pending_updates(), 1);
}

#[tokio::test]
async fn offset_advances_only_past_returned_updates() {
    let (fake, tg) = setup().await;
    let first = fake.push_text(1, 1, "one");
    let second = fake.push_text(1, 1, "two");
    // A failed poll returns nothing and must not move the offset.
    fake.forbid_once();
    assert!(tg.next().await.is_err());
    let got = tg.next().await.unwrap();
    assert_eq!(got.len(), 2, "updates are replayed after a failed poll");
    assert!(fake
        .poll_requests()
        .iter()
        .all(|p| p.get("offset").is_none()));
    // The next poll acknowledges both.
    fake.push_text(1, 1, "three");
    let got = tg.next().await.unwrap();
    assert_eq!(got.len(), 1);
    let polls = fake.poll_requests();
    assert_eq!(polls.last().unwrap()["offset"], second + 1);
    assert!(first < second);
}

#[tokio::test]
async fn command_suffix_is_stripped_only_for_commands() {
    let (fake, tg) = setup().await;
    fake.push_text(1, 1, "/new@my_bot");
    fake.push_text(1, 1, "/cancel@my_bot now");
    fake.push_text(1, 1, "mail me@example.com");
    let texts: Vec<String> = tg
        .next()
        .await
        .unwrap()
        .into_iter()
        .map(|i| match i {
            Inbound::Message { text, .. } => text,
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert_eq!(texts, vec!["/new", "/cancel now", "mail me@example.com"]);
}

#[tokio::test]
async fn long_poll_times_out_with_an_empty_batch() {
    let (fake, tg) = setup().await;
    let start = Instant::now();
    let got = tg.next().await.unwrap();
    assert!(got.is_empty());
    assert!(start.elapsed() >= Duration::from_millis(900));
    assert_eq!(fake.poll_requests().len(), 1);
}

#[tokio::test]
async fn long_poll_wakes_on_a_new_update() {
    let (fake, tg) = setup().await;
    let start = Instant::now();
    let (got, ()) = tokio::join!(tg.next(), async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        fake.push_text(3, 3, "late");
    });
    assert_eq!(got.unwrap().len(), 1);
    assert!(start.elapsed() < Duration::from_millis(900));
}

#[tokio::test]
async fn send_text_plain_no_parse_mode() {
    let (fake, tg) = setup().await;
    let r = tg.send_text(ChatId(9), "a_b *c* [d](e) `f`").await.unwrap();
    assert_eq!(r.chat, ChatId(9));
    assert_eq!(r.message_id, 1000);
    let sent = fake.sent_messages();
    assert_eq!(sent[0]["chat_id"], 9);
    assert_eq!(sent[0]["text"], "a_b *c* [d](e) `f`");
    assert!(sent[0].get("parse_mode").is_none());
}

#[tokio::test]
async fn blank_text_is_replaced_by_a_placeholder() {
    let (fake, tg) = setup().await;
    tg.send_text(ChatId(9), "   ").await.unwrap();
    assert_eq!(sent_texts(&fake), vec!["(empty message)"]);
}

#[tokio::test]
async fn splits_ascii_and_returns_last_part_ref() {
    let (fake, tg) = setup().await;
    let line = "x".repeat(99);
    let text = vec![line.as_str(); 100].join("\n");
    let r = tg.send_text(ChatId(9), &text).await.unwrap();
    let parts = sent_texts(&fake);
    assert!(parts.len() >= 3);
    assert!(parts.iter().all(|p| utf16(p) <= 4096));
    assert_eq!(parts.concat(), text);
    assert_eq!(r.message_id, 1000 + parts.len() as i64 - 1);
}

#[tokio::test]
async fn splits_multibyte_text_safely() {
    let (fake, tg) = setup().await;
    let text = "aé漢😀".repeat(3000);
    tg.send_text(ChatId(9), &text).await.unwrap();
    let parts = sent_texts(&fake);
    assert!(parts.len() >= 2);
    assert!(parts.iter().all(|p| utf16(p) <= 4096));
    assert_eq!(parts.concat(), text);
}

#[tokio::test]
async fn splits_a_single_very_long_line() {
    let (fake, tg) = setup().await;
    let text = "q".repeat(9000);
    tg.send_text(ChatId(9), &text).await.unwrap();
    let parts = sent_texts(&fake);
    assert_eq!(parts.len(), 3);
    assert_eq!(parts.concat(), text);
}

fn prompt(data: &str) -> ApprovalPrompt {
    ApprovalPrompt {
        text: "Allow bash?".into(),
        buttons: vec![vec![
            Button {
                label: "Allow".into(),
                data: data.into(),
            },
            Button {
                label: "Deny".into(),
                data: "d".into(),
            },
        ]],
    }
}

#[tokio::test]
async fn approval_builds_an_inline_keyboard() {
    let (fake, tg) = setup().await;
    let data = "a".repeat(64);
    let r = tg.send_approval(ChatId(9), &prompt(&data)).await.unwrap();
    assert_eq!(r.message_id, 1000);
    let sent = &fake.sent_messages()[0];
    let kb = &sent["reply_markup"]["inline_keyboard"];
    assert_eq!(kb[0][0]["text"], "Allow");
    assert_eq!(kb[0][0]["callback_data"], data);
    assert_eq!(kb[0][1]["callback_data"], "d");
}

#[tokio::test]
async fn approval_rejects_oversized_button_data() {
    let (fake, tg) = setup().await;
    let err = tg
        .send_approval(ChatId(9), &prompt(&"a".repeat(65)))
        .await
        .unwrap_err();
    assert!(matches!(err, ChatError::Api(_)), "{err:?}");
    // 64 bytes, not characters: 33 two-byte chars are 66 bytes.
    let err = tg
        .send_approval(ChatId(9), &prompt(&"é".repeat(33)))
        .await
        .unwrap_err();
    assert!(matches!(err, ChatError::Api(_)));
    assert!(fake.sent_messages().is_empty());
}

#[tokio::test]
async fn finish_approval_edits_and_drops_the_keyboard() {
    let (fake, tg) = setup().await;
    let msg = MessageRef {
        chat: ChatId(9),
        message_id: 55,
    };
    tg.finish_approval(msg, "Allowed").await.unwrap();
    let e = &fake.edits()[0];
    assert_eq!(e["chat_id"], 9);
    assert_eq!(e["message_id"], 55);
    assert_eq!(e["text"], "Allowed");
    assert_eq!(e["reply_markup"], json!({"inline_keyboard": []}));
}

#[tokio::test]
async fn finish_approval_tolerates_not_modified() {
    let (fake, tg) = setup().await;
    fake.reject_once("Bad Request: message is not modified");
    let msg = MessageRef {
        chat: ChatId(9),
        message_id: 55,
    };
    tg.finish_approval(msg, "same").await.unwrap();
}

#[tokio::test]
async fn typing_answer_and_commands() {
    let (fake, tg) = setup().await;
    tg.typing(ChatId(9)).await.unwrap();
    assert_eq!(
        fake.chat_actions()[0],
        json!({"chat_id": 9, "action": "typing"})
    );
    tg.answer_callback("cb1", Some("done")).await.unwrap();
    tg.answer_callback("cb2", None).await.unwrap();
    let a = fake.callback_answers();
    assert_eq!(a[0], json!({"callback_query_id": "cb1", "text": "done"}));
    assert_eq!(a[1], json!({"callback_query_id": "cb2"}));
    tg.register_commands().await.unwrap();
    let names: Vec<String> = fake.commands()[0]["commands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["command"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, vec!["new", "cancel", "status", "help"]);
}

#[tokio::test]
async fn maps_rate_limit_gone_api_and_decode() {
    let (fake, tg) = setup().await;
    fake.rate_limit_once(3);
    assert!(matches!(
        tg.send_text(ChatId(1), "x").await,
        Err(ChatError::RateLimited {
            retry_after_secs: 3
        })
    ));
    fake.forbid_once();
    assert!(matches!(
        tg.send_text(ChatId(1), "x").await,
        Err(ChatError::Gone)
    ));
    fake.reject_once("Bad Request: chat not found");
    assert!(matches!(tg.typing(ChatId(1)).await, Err(ChatError::Gone)));
    fake.reject_once("Bad Request: message text is empty");
    match tg.send_text(ChatId(1), "x").await {
        Err(ChatError::Api(m)) => assert!(m.contains("message text is empty")),
        other => panic!("{other:?}"),
    }
    fake.raw_once(200, "<<not json>>");
    assert!(matches!(
        tg.send_text(ChatId(1), "x").await,
        Err(ChatError::Decode(_))
    ));
    // Everything still works afterwards.
    tg.send_text(ChatId(1), "x").await.unwrap();
}

#[tokio::test]
async fn token_never_appears_in_errors_or_debug() {
    // A closed port gives a genuine transport error whose raw text would carry
    // the URL, and so the token.
    let closed = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", l.local_addr().unwrap())
    };
    let tg = TelegramAdapter::new(config(closed.clone(), 1)).unwrap();
    let mut strings = Vec::new();
    for err in [
        tg.send_text(ChatId(1), "x").await.unwrap_err(),
        tg.next().await.unwrap_err(),
        tg.typing(ChatId(1)).await.unwrap_err(),
    ] {
        assert!(matches!(err, ChatError::Transport(_)), "{err:?}");
        strings.push(err.to_string());
        strings.push(format!("{err:?}"));
        strings.push(format!("{err:#?}"));
    }
    // Server-side error texts as well.
    let (fake, tg2) = setup().await;
    fake.reject_once("Bad Request: something");
    strings.push(format!(
        "{:?}",
        tg2.send_text(ChatId(1), "x").await.unwrap_err()
    ));
    strings.push(format!("{tg:?} {tg2:?}"));
    strings.push(format!("{:?}", config(fake.url(), 1)));
    for s in &strings {
        assert!(!s.contains("PLANTED"), "token leaked: {s}");
        assert!(!s.contains(TOKEN), "token leaked: {s}");
        assert!(!s.contains("/bot"), "url leaked: {s}");
        assert!(
            !s.contains(closed.trim_start_matches("http://")),
            "host leaked: {s}"
        );
    }
}

#[tokio::test]
async fn updates_flow_through_the_adapter_in_order() {
    let (fake, tg) = setup().await;
    for i in 0..5 {
        fake.push_text(1, 1, &format!("m{i}"));
    }
    let got = tg.next().await.unwrap();
    let texts: Vec<_> = got
        .iter()
        .map(|i| match i {
            Inbound::Message { text, .. } => text.clone(),
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(texts, vec!["m0", "m1", "m2", "m3", "m4"]);
}
