//! Update parsing and normalization.

use serde::Deserialize;
use serde_json::Value;

use crate::channel::{ChatId, Inbound, MessageRef, UserId};

#[derive(Deserialize)]
struct Update {
    message: Option<Message>,
    callback_query: Option<CallbackQuery>,
}

#[derive(Deserialize)]
struct Message {
    message_id: i64,
    from: Option<User>,
    chat: Chat,
    text: Option<String>,
}

#[derive(Deserialize)]
struct Chat {
    id: i64,
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Deserialize)]
struct User {
    id: i64,
    #[serde(default)]
    is_bot: bool,
}

#[derive(Deserialize)]
struct CallbackQuery {
    id: String,
    from: User,
    message: Option<Message>,
    data: Option<String>,
}

/// The highest `update_id` in a batch plus one, i.e. the next offset.
pub(crate) fn next_offset(updates: &[Value]) -> Option<i64> {
    updates
        .iter()
        .filter_map(|u| u.get("update_id").and_then(Value::as_i64))
        .max()
        .map(|id| id + 1)
}

/// Normalize one raw update. `None` means "not for the app": edited messages,
/// channel posts, groups, stickers, anything that is not private text or a
/// button press. The caller still advances the offset past it.
pub(crate) fn normalize(raw: &Value) -> Option<Inbound> {
    let update: Update = serde_json::from_value(raw.clone()).ok()?;
    if let Some(m) = update.message {
        if m.chat.kind != "private" {
            return None;
        }
        let from = m.from?;
        if from.is_bot {
            return None;
        }
        let text = m.text?;
        return Some(Inbound::Message {
            chat: ChatId(m.chat.id),
            user: UserId(from.id),
            text: strip_command_suffix(&text),
            message_id: m.message_id,
        });
    }
    if let Some(cb) = update.callback_query {
        if cb.from.is_bot {
            return None;
        }
        let data = cb.data?;
        // A callback without a message (inline mode, or a message too old for
        // Telegram to include) cannot be tied to a verified private chat, and
        // this bot only attaches keyboards to its own messages: drop it
        // instead of guessing the chat from the user id.
        let m = cb.message?;
        if m.chat.kind != "private" {
            return None;
        }
        let message = MessageRef {
            chat: ChatId(m.chat.id),
            message_id: m.message_id,
        };
        let chat = message.chat;
        return Some(Inbound::Callback {
            chat,
            user: UserId(cb.from.id),
            callback_id: cb.id,
            data,
            message: Some(message),
        });
    }
    None
}

/// `/new@my_bot arg` becomes `/new arg`. Text that is not a command is
/// returned untouched, and so is everything after the command word.
pub(crate) fn strip_command_suffix(text: &str) -> String {
    if !text.starts_with('/') {
        return text.to_string();
    }
    let word_end = text.find(char::is_whitespace).unwrap_or(text.len());
    let (word, rest) = text.split_at(word_end);
    match word.find('@') {
        Some(at) if at > 1 => format!("{}{}", &word[..at], rest),
        _ => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn strips_suffix_only_on_commands() {
        assert_eq!(strip_command_suffix("/new@my_bot"), "/new");
        assert_eq!(strip_command_suffix("/new@my_bot  now"), "/new  now");
        assert_eq!(strip_command_suffix("/new"), "/new");
        assert_eq!(strip_command_suffix("mail me@x.com"), "mail me@x.com");
        assert_eq!(strip_command_suffix("/@bot"), "/@bot");
        assert_eq!(strip_command_suffix("hi /new@my_bot"), "hi /new@my_bot");
        assert_eq!(strip_command_suffix("/say hi@bot"), "/say hi@bot");
    }

    #[test]
    fn private_text_is_a_message() {
        let u = json!({"update_id":1,"message":{"message_id":5,"from":{"id":9,"is_bot":false},
            "chat":{"id":9,"type":"private"},"text":"hello"}});
        assert_eq!(
            normalize(&u),
            Some(Inbound::Message {
                chat: ChatId(9),
                user: UserId(9),
                text: "hello".into(),
                message_id: 5
            })
        );
    }

    #[test]
    fn ignored_kinds() {
        let group = json!({"update_id":1,"message":{"message_id":5,"from":{"id":9},
            "chat":{"id":-100,"type":"supergroup"},"text":"hello"}});
        let sticker = json!({"update_id":2,"message":{"message_id":5,"from":{"id":9},
            "chat":{"id":9,"type":"private"},"sticker":{}}});
        let edited = json!({"update_id":3,"edited_message":{"message_id":5,"from":{"id":9},
            "chat":{"id":9,"type":"private"},"text":"x"}});
        let channel = json!({"update_id":4,"channel_post":{"message_id":5,
            "chat":{"id":-5,"type":"channel"},"text":"x"}});
        let bot = json!({"update_id":5,"message":{"message_id":5,"from":{"id":9,"is_bot":true},
            "chat":{"id":9,"type":"private"},"text":"x"}});
        for u in [group, sticker, edited, channel, bot] {
            assert_eq!(normalize(&u), None, "{u}");
        }
    }

    #[test]
    fn callback_carries_the_message() {
        let u = json!({"update_id":1,"callback_query":{"id":"cb1","from":{"id":9,"is_bot":false},
            "message":{"message_id":77,"chat":{"id":9,"type":"private"},"date":0},"data":"a:1"}});
        assert_eq!(
            normalize(&u),
            Some(Inbound::Callback {
                chat: ChatId(9),
                user: UserId(9),
                callback_id: "cb1".into(),
                data: "a:1".into(),
                message: Some(MessageRef {
                    chat: ChatId(9),
                    message_id: 77
                }),
            })
        );
        let no_msg =
            json!({"update_id":2,"callback_query":{"id":"cb2","from":{"id":9},"data":"d"}});
        assert_eq!(normalize(&no_msg), None);
        let no_data = json!({"update_id":3,"callback_query":{"id":"cb3","from":{"id":9}}});
        assert_eq!(normalize(&no_data), None);
    }

    #[test]
    fn offset_is_max_plus_one() {
        assert_eq!(next_offset(&[]), None);
        let v = vec![
            json!({"update_id":4}),
            json!({"update_id":9}),
            json!({"update_id":6}),
        ];
        assert_eq!(next_offset(&v), Some(10));
    }
}
