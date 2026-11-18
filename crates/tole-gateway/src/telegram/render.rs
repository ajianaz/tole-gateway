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
#[cfg(test)]
fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// Most parts one message may be split into. Longer text is cut and ends
/// with a notice naming how much was left out.
pub(crate) const MAX_PARTS: usize = 20;

/// End (byte offset) of the next part of `rest`: at most [`MAX_LEN`] UTF-16
/// units, preferring a line break, then a space, then a hard cut. Only the
/// window of at most `MAX_LEN + 1` units is scanned, never the whole rest.
fn next_cut(rest: &str) -> usize {
    let mut units = 0;
    let mut hard_end = None;
    for (i, c) in rest.char_indices() {
        units += c.len_utf16();
        if units > MAX_LEN {
            hard_end = Some(i);
            break;
        }
    }
    let Some(hard_end) = hard_end else {
        return rest.len();
    };
    let window = &rest[..hard_end];
    window
        .rfind('\n')
        .map(|i| i + 1)
        .or_else(|| {
            window
                .rfind(char::is_whitespace)
                .map(|i| i + window[i..].chars().next().map_or(1, char::len_utf8))
        })
        .filter(|&c| c > 0)
        .unwrap_or(hard_end)
}

/// Split `text` into parts of at most [`MAX_LEN`] UTF-16 units, preferring a
/// line break, then a space, then a hard cut on a character boundary.
/// Whitespace-only parts are dropped, so blank input gives no parts; the parts
/// of non-blank input concatenate back to the input (up to dropped blanks).
/// At most [`MAX_PARTS`] parts come back: when the text needs more, the first
/// `MAX_PARTS - 1` are kept and the last part is a short notice with the
/// number of characters left out. Cost is linear in the text length.
pub(crate) fn split_message(text: &str) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();
    // Start of the text that went into the last part, to count what a cut drops.
    let mut last_start = 0;
    let mut pos = 0;
    while pos < text.len() {
        let rest = &text[pos..];
        let cut = next_cut(rest);
        if !rest[..cut].trim().is_empty() {
            if parts.len() == MAX_PARTS {
                // One part too many: drop the last kept part, say so instead.
                parts.pop();
                let dropped = text[last_start..].chars().count();
                parts.push(format!("[output truncated: {dropped} more characters]"));
                return parts;
            }
            last_start = pos;
            parts.push(rest[..cut].to_string());
        }
        pos += cut;
    }
    parts
}

/// `text` as one message: unchanged when it fits, otherwise cut at a char
/// boundary and ended with an ellipsis, so the result is at most [`MAX_LEN`]
/// UTF-16 units.
pub(crate) fn truncate_one(text: &str) -> String {
    if text.encode_utf16().take(MAX_LEN + 1).count() <= MAX_LEN {
        return text.to_string();
    }
    // Over the limit: keep room for the one-unit ellipsis.
    let mut units = 0;
    let mut end = 0;
    for (i, c) in text.char_indices() {
        units += c.len_utf16();
        if units > MAX_LEN - 1 {
            break;
        }
        end = i + c.len_utf8();
    }
    format!("{}\u{2026}", &text[..end])
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
    fn parts_are_capped_with_a_notice() {
        // 25 full parts of hard-cut text.
        let text = "z".repeat(MAX_LEN * 25);
        let parts = split_message(&text);
        assert_eq!(parts.len(), MAX_PARTS);
        let kept = MAX_PARTS - 1;
        for p in &parts[..kept] {
            assert_eq!(p.len(), MAX_LEN);
        }
        let left = MAX_LEN * 25 - MAX_LEN * kept;
        assert_eq!(
            parts[kept],
            format!("[output truncated: {left} more characters]")
        );
        // Exactly MAX_PARTS parts is not truncated.
        let exact = "z".repeat(MAX_LEN * MAX_PARTS);
        let parts = split_message(&exact);
        assert_eq!(parts.len(), MAX_PARTS);
        assert!(parts.iter().all(|p| p.len() == MAX_LEN));
    }

    #[test]
    fn huge_text_splits_in_linear_time() {
        let text = "ab cd\n".repeat(2_000_000);
        let t = std::time::Instant::now();
        let parts = split_message(&text);
        assert_eq!(parts.len(), MAX_PARTS);
        assert!(t.elapsed() < std::time::Duration::from_secs(2));
    }

    #[test]
    fn truncate_one_fits_or_ends_with_ellipsis() {
        assert_eq!(truncate_one("hello"), "hello");
        assert_eq!(truncate_one(""), "");
        let exact = "a".repeat(MAX_LEN);
        assert_eq!(truncate_one(&exact), exact);
        let long = "😀".repeat(5000);
        let t = truncate_one(&long);
        assert!(utf16_len(&t) <= MAX_LEN);
        assert!(t.ends_with('\u{2026}'));
        let t = truncate_one(&"a".repeat(MAX_LEN + 1));
        assert_eq!(utf16_len(&t), MAX_LEN);
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
