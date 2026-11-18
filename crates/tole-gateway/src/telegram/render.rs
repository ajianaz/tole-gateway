//! Message splitting.
//!
//! Rendering choice: plain text, no `parse_mode`. Tool output, file paths and
//! diffs are full of `_ * [ ] ( ) ~ ` > # + - = | { } . !`; every one of them is
//! special in MarkdownV2 and one missed escape makes Telegram reject the whole
//! message. Plain text cannot fail that way, so it is the safe choice.

/// Telegram's per-message limit.
pub(crate) const MAX_LEN: usize = 4096;

/// Length in UTF-16 code units. Telegram counts text after parsing, and UTF-16
/// units are never smaller than characters, so staying under the limit in
/// units is safe under either reading of "4096 characters".
fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// Split `text` into parts of at most [`MAX_LEN`] UTF-16 units, preferring a
/// line break, then a space, then a hard cut on a character boundary.
/// Whitespace-only parts are dropped, so blank input gives no parts; the parts
/// of non-blank input concatenate back to the input (up to dropped blanks).
pub(crate) fn split_message(text: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        if utf16_len(rest) <= MAX_LEN {
            push_part(&mut parts, rest);
            break;
        }
        let mut units = 0;
        let mut hard_end = rest.len();
        for (i, c) in rest.char_indices() {
            units += c.len_utf16();
            if units > MAX_LEN {
                hard_end = i;
                break;
            }
        }
        let window = &rest[..hard_end];
        let cut = window
            .rfind('\n')
            .map(|i| i + 1)
            .or_else(|| {
                window
                    .rfind(char::is_whitespace)
                    .map(|i| i + window[i..].chars().next().map_or(1, char::len_utf8))
            })
            .filter(|&c| c > 0)
            .unwrap_or(hard_end);
        push_part(&mut parts, &rest[..cut]);
        rest = &rest[cut..];
    }
    parts
}

fn push_part(parts: &mut Vec<String>, s: &str) {
    if !s.trim().is_empty() {
        parts.push(s.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(text: &str) -> Vec<String> {
        let parts = split_message(text);
        for p in &parts {
            assert!(utf16_len(p) <= MAX_LEN, "part too long: {}", utf16_len(p));
            assert!(!p.is_empty());
        }
        assert_eq!(parts.concat(), text);
        parts
    }

    #[test]
    fn short_and_blank() {
        assert_eq!(check("hello"), vec!["hello"]);
        assert!(split_message("").is_empty());
        assert!(split_message("  \n ").is_empty());
    }

    #[test]
    fn exact_limit_is_one_part() {
        assert_eq!(check(&"a".repeat(MAX_LEN)).len(), 1);
        assert_eq!(check(&"a".repeat(MAX_LEN + 1)).len(), 2);
    }

    #[test]
    fn splits_at_line_boundaries() {
        let line = "x".repeat(99);
        let text = (0..100)
            .map(|_| line.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let parts = check(&text);
        assert!(parts.len() >= 3);
        for p in &parts[..parts.len() - 1] {
            assert!(p.ends_with('\n'));
        }
    }

    #[test]
    fn splits_at_word_boundaries() {
        let text = "word ".repeat(2000);
        let parts = check(&text);
        assert!(parts.len() >= 3);
        for p in &parts[..parts.len() - 1] {
            assert!(p.ends_with(' '));
        }
    }

    #[test]
    fn single_very_long_line_is_hard_cut() {
        let text = "z".repeat(10_000);
        let parts = check(&text);
        assert_eq!(parts.len(), 3);
    }

    #[test]
    fn multibyte_never_cut_inside_a_char() {
        // 3-byte chars and 4-byte (surrogate pair) chars.
        let cjk = "漢".repeat(5000);
        check(&cjk);
        let emoji = "😀".repeat(5000);
        let parts = check(&emoji);
        for p in &parts {
            assert!(utf16_len(p) <= MAX_LEN);
        }
        let mixed = "aé漢😀".repeat(3000);
        check(&mixed);
    }
}
