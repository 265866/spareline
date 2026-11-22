//! Text helpers that follow JavaScript's string and regular expression semantics, so the parsers
//! ported from spareline-old treat white space, line ends and integers exactly as it did.

/// Whether JavaScript's `\s` and `String.prototype.trim` count `c` as white space: Unicode's
/// space separators, the ASCII controls tab to carriage return, the byte order mark and the line
/// terminators.
pub(crate) fn is_space(c: char) -> bool {
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

/// Whether `c` ends a line for a JavaScript regular expression, which `.` doesn't match.
pub(crate) fn is_line_terminator(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

/// Whether `c` is a word character for JavaScript's `\w` and `\b`: an ASCII letter, digit or `_`.
pub(crate) fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// `text.trim()` in JavaScript.
pub(crate) fn trim(text: &str) -> &str {
    text.trim_matches(is_space)
}

/// `text.trimStart()` in JavaScript.
pub(crate) fn trim_start(text: &str) -> &str {
    text.trim_start_matches(is_space)
}

/// `text.trimEnd()` in JavaScript.
pub(crate) fn trim_end(text: &str) -> &str {
    text.trim_end_matches(is_space)
}

/// The lines of `text` as `text.split(/\r?\n/)` gives them, an empty last one included.
pub(crate) fn lines(text: &str) -> impl Iterator<Item = &str> {
    text.split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
}

/// The integer `text` spells when it matches `/^-?\d+$/` exactly. A value that matches but
/// overflows `i64` is `None`, where JavaScript would read an inexact float.
pub(crate) fn parse_int(text: &str) -> Option<i64> {
    let digits = text.strip_prefix('-').unwrap_or(text);
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// The run of ASCII digits that starts `text`, possibly empty.
pub(crate) fn leading_digits(text: &str) -> &str {
    let end = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    &text[..end]
}

/// Whether the number after a prefix may start with a minus sign.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Sign {
    /// Digits only, as `(\d+)`.
    Unsigned,
    /// An optional minus sign, then digits, as `(-?\d+)`.
    Signed,
}

/// Where the prefix may start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Start {
    /// Anywhere in the text.
    Anywhere,
    /// Only where `\b` holds before it: at the start of the text or after a non-word character.
    WordStart,
}

/// The number in the first match of `/<prefix>(\d+)/` (or `(-?\d+)` when signed, and with `\b`
/// before the prefix for [`Start::WordStart`]), as the text of the captured group.
pub(crate) fn number_after<'t>(
    text: &'t str,
    prefix: &str,
    sign: Sign,
    start: Start,
) -> Option<&'t str> {
    for index in occurrences(text, prefix) {
        if start == Start::WordStart && !word_boundary_before(text, index) {
            continue;
        }
        if let Some(number) = leading_number(&text[index + prefix.len()..], sign) {
            return Some(number);
        }
    }
    None
}

/// The number that starts `text`, as `^(\d+)` or `^(-?\d+)` captures it.
pub(crate) fn leading_number(text: &str, sign: Sign) -> Option<&str> {
    let unsigned = match sign {
        Sign::Signed => text.strip_prefix('-').unwrap_or(text),
        Sign::Unsigned => text,
    };
    let digits = leading_digits(unsigned);
    if digits.is_empty() {
        return None;
    }
    Some(&text[..text.len() - unsigned.len() + digits.len()])
}

/// Whether `\b` holds at `index` before a word character: the text starts there, or the
/// character before it isn't a word character.
pub(crate) fn word_boundary_before(text: &str, index: usize) -> bool {
    text[..index]
        .chars()
        .next_back()
        .is_none_or(|c| !is_word(c))
}

/// The run that starts `text` of characters other than white space, as `(\S+)` captures it.
pub(crate) fn leading_non_space(text: &str) -> &str {
    let end = text.find(is_space).unwrap_or(text.len());
    &text[..end]
}

/// The start of every occurrence of `pattern` in `text`, overlapping ones included, in order, as a
/// regular expression's search tries each start position in turn.
pub(crate) fn occurrences<'t>(text: &'t str, pattern: &'t str) -> impl Iterator<Item = usize> + 't {
    text.char_indices()
        .map(|(index, _)| index)
        .filter(move |&index| text[index..].starts_with(pattern))
}
