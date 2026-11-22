//! String rules from JavaScript that the API's validation followed in spareline-old.

/// Whether JavaScript's `String.prototype.trim` counts `c` as white space.
fn is_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

/// `text.trim()` in JavaScript.
pub(crate) fn trim(text: &str) -> &str {
    text.trim_matches(is_space)
}

/// `text.length` in JavaScript: the number of UTF-16 code units, which zod's `min` and `max`
/// count.
pub(crate) fn length(text: &str) -> usize {
    text.encode_utf16().count()
}

/// `Number(text)` in JavaScript, for the integers a query or a path may carry: blank text is 0,
/// `0x`, `0o` and `0b` prefixes name other bases, and decimal text may have a sign, a fraction
/// and an exponent. `None` for anything that isn't a whole number in `i64`'s range, such as
/// `1.5`, `abc` or `Infinity`, which zod's `int()` refuses.
pub(crate) fn whole_number(text: &str) -> Option<i64> {
    let trimmed = trim(text);
    if trimmed.is_empty() {
        return Some(0);
    }
    let prefixed = [
        ("0x", 16),
        ("0X", 16),
        ("0o", 8),
        ("0O", 8),
        ("0b", 2),
        ("0B", 2),
    ];
    for (prefix, radix) in prefixed {
        if let Some(digits) = trimmed.strip_prefix(prefix) {
            if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
                return None;
            }
            return i64::from_str_radix(digits, radix).ok();
        }
    }
    let decimal = trimmed
        .chars()
        .all(|c| c.is_ascii_digit() || matches!(c, '+' | '-' | '.' | 'e' | 'E'));
    if !decimal {
        return None;
    }
    let value: f64 = trimmed.parse().ok()?;
    float_to_whole(value)
}

/// `value` as an `i64` when it is a whole number that `i64` holds exactly enough.
#[expect(
    clippy::cast_possible_truncation,
    reason = "the value is checked to be whole and inside i64's range before the cast"
)]
fn float_to_whole(value: f64) -> Option<i64> {
    let limit = 9_223_372_036_854_775_807.0_f64;
    (value.is_finite() && value.fract() == 0.0 && value.abs() < limit).then_some(value as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_numbers_as_javascript_does() {
        for (text, expected) in [
            ("5", Some(5)),
            (" 5 ", Some(5)),
            ("", Some(0)),
            ("  ", Some(0)),
            ("1e2", Some(100)),
            ("+7", Some(7)),
            ("-3", Some(-3)),
            ("2.0", Some(2)),
            ("0x10", Some(16)),
            ("0b11", Some(3)),
            ("1.5", None),
            ("abc", None),
            ("Infinity", None),
            ("inf", None),
            ("NaN", None),
            ("0x", None),
            ("1_000", None),
        ] {
            assert_eq!(whole_number(text), expected, "{text:?}");
        }
    }

    #[test]
    fn counts_and_trims_as_javascript_does() {
        assert_eq!(length("café"), 4);
        assert_eq!(length("\u{1f600}"), 2, "an emoji is two UTF-16 code units");
        assert_eq!(trim("\u{feff} a \u{3000}"), "a");
    }
}
