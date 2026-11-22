//! String handling that matches what JavaScript does, where spareline-old relied on it.

/// Whether `character` is whitespace to JavaScript's `String.prototype.trim` and to `\s` in a
/// regular expression: the `WhiteSpace` and `LineTerminator` code points of ECMAScript. This
/// differs from Rust's `char::is_whitespace`, which also counts U+0085 and doesn't count U+FEFF.
pub(crate) fn is_whitespace(character: char) -> bool {
    matches!(
        character,
        '\u{9}' | '\u{a}' | '\u{b}' | '\u{c}' | '\u{d}' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

/// `text` without leading and trailing whitespace, as JavaScript's `trim()` returns it.
pub(crate) fn trim(text: &str) -> &str {
    text.trim_matches(is_whitespace)
}

/// The length of `text` as JavaScript counts it, in UTF-16 code units, so a character outside
/// the Basic Multilingual Plane, such as an emoji, counts twice.
pub(crate) fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_what_javascript_trims() {
        assert_eq!(trim("\u{feff}\u{a0} alice\u{3000}\n"), "alice");
        assert_eq!(
            trim("\u{85}alice"),
            "\u{85}alice",
            "U+0085 isn't whitespace to JavaScript"
        );
        assert_eq!(trim(""), "");
    }

    #[test]
    fn counts_utf16_code_units() {
        assert_eq!(utf16_len("abc"), 3);
        assert_eq!(utf16_len("\u{1f600}"), 2);
        assert_eq!(utf16_len("\u{e9}"), 1);
    }
}
