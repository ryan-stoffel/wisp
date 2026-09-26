//! A tiny shared helper for counting a JSON string's size the way `serde_json` actually writes
//! it, used wherever a byte budget has to hold after encoding, not just before it (#157, #190).

/// The size of `text` as a JSON string's content, as `serde_json` writes it: `"` and `\\` and the
/// short escapes (`\n`, `\t`, ...) take two bytes, other control characters six (`\u00XX`).
pub(crate) fn escaped_len(text: &str) -> usize {
    text.bytes()
        .map(|byte| match byte {
            b'"' | b'\\' | b'\n' | b'\r' | b'\t' | 0x08 | 0x0c => 2,
            0x00..=0x1f => 6,
            _ => 1,
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::escaped_len;

    #[test]
    fn escaped_len_counts_escapes_as_serde_json_writes_them() {
        for text in [
            "plain",
            "quote \" and \\",
            "tab\tnew\nline\r",
            "\u{1}\u{1f}\u{7f}",
            "é✓",
            "",
        ] {
            let encoded = serde_json::to_string(text).unwrap();
            assert_eq!(escaped_len(text), encoded.len() - 2, "{text:?}");
        }
    }
}
