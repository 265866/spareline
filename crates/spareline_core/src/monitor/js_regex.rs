//! Character classes that make a `regex` pattern match what a JavaScript regular expression
//! without the `u` flag matched, for the patterns ported from spareline-old.

use regex::Regex;

/// JavaScript's `\s`: Unicode's space separators, tab to carriage return, the byte order mark and
/// the line terminators, but not U+0085, which the `regex` crate's `\s` includes.
pub(crate) const SPACE: &str = r"[\t\n\x0B\x0C\r \u{A0}\u{1680}\u{2000}-\u{200A}\u{2028}\u{2029}\u{202F}\u{205F}\u{3000}\u{FEFF}]";

/// JavaScript's `\S`.
pub(crate) const NON_SPACE: &str = r"[^\t\n\x0B\x0C\r \u{A0}\u{1680}\u{2000}-\u{200A}\u{2028}\u{2029}\u{202F}\u{205F}\u{3000}\u{FEFF}]";

/// JavaScript's `[^,\s]`.
pub(crate) const NON_COMMA_NON_SPACE: &str = r"[^,\t\n\x0B\x0C\r \u{A0}\u{1680}\u{2000}-\u{200A}\u{2028}\u{2029}\u{202F}\u{205F}\u{3000}\u{FEFF}]";

/// JavaScript's `.`, which stops at every line terminator, not only at `\n`.
pub(crate) const DOT: &str = r"[^\n\r\u{2028}\u{2029}]";

/// JavaScript's `\b`, which counts only ASCII letters, digits and `_` as word characters.
pub(crate) const BOUNDARY: &str = r"(?-u:\b)";

/// A pattern written with `{s}` for JavaScript's `\s`, `{S}` for `\S`, `{b}` for `\b`, `{dot}` for
/// `.` and `{nc}` for `[^,\s]`, compiled.
pub(crate) fn pattern(template: &str) -> Regex {
    let source = template
        .replace("{s}", SPACE)
        .replace("{S}", NON_SPACE)
        .replace("{b}", BOUNDARY)
        .replace("{dot}", DOT)
        .replace("{nc}", NON_COMMA_NON_SPACE);
    Regex::new(&source).expect("the ported patterns are valid")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_classes_match_what_javascript_matched() {
        let space = pattern("^{s}$");
        for c in [
            ' ', '\t', '\u{b}', '\u{a0}', '\u{feff}', '\u{2028}', '\u{3000}',
        ] {
            assert!(space.is_match(&c.to_string()), "{c:?}");
        }
        assert!(!space.is_match("\u{85}"));
        assert!(pattern("^{S}$").is_match("\u{85}"));
        assert!(!pattern("^{dot}$").is_match("\r"));
        assert!(pattern("{b}x").is_match("éx"));
        assert!(!pattern("^{nc}$").is_match(","));
    }
}
