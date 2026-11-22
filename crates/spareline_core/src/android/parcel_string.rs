//! The reply of `service call` for a binder method that returns a string.

use crate::text;

const PARCEL: &str = "Result: Parcel(";

/// What a `service call` reply holds, for a binder method that returns a `String`, such as
/// `IPhoneSubInfo`'s getters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParcelString {
    /// The method returned; `None` is a null string.
    Value(Option<String>),
    /// The method threw: the exception code, and the message when the reply holds one.
    Exception {
        /// The `Parcel::writeException` code, such as -1 for `EX_SECURITY`.
        code: i32,
        /// The exception's message.
        message: Option<String>,
    },
    /// The output isn't a reply this can read, such as an error from the tool itself.
    Invalid {
        /// Why, such as the tool's own error line.
        reason: String,
    },
}

/// Reads the output of `service call <service> <code> ...` for a method that returns a string.
///
/// The tool prints the reply `Parcel` as 32-bit little-endian words in hex, several lines with
/// offsets, or one line for a short reply:
///
/// ```text
/// Result: Parcel(
/// 0x00000000: 00000000 0000000f 00350033 00300030 '........3.5.0.0.'
/// 0x00000010: ...                                 '...)
/// Result: Parcel(<tab>00000000 ffffffff   '........')
/// ```
///
/// The reply is an int32 exception code (0 for none), then a `String16`: an int32 length in UTF-16
/// code units (-1 for null) and the code units. A code unit that isn't valid UTF-16, such as a
/// lone surrogate, reads as U+FFFD.
#[must_use]
pub fn parse_parcel_string(output: &str) -> ParcelString {
    let Some(start) = output.find(PARCEL) else {
        let first = text::trim(output).split('\n').next().map_or("", text::trim);
        return invalid(if first.is_empty() {
            "no Parcel in output"
        } else {
            first
        });
    };
    let reply = &output[start + PARCEL.len()..];
    let mut bytes: Vec<u8> = Vec::new();
    for raw_line in reply.split('\n') {
        for word in hex_words(text::trim_start(without_offset(raw_line))) {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        if text::trim_end(raw_line).ends_with("')") {
            break;
        }
    }
    let Some(code) = read_i32(&bytes, 0) else {
        let detail = text::trim(reply);
        let detail = detail.strip_suffix(')').unwrap_or(detail);
        return invalid(if detail.is_empty() {
            "empty Parcel"
        } else {
            detail
        });
    };
    let string = read_string16(&bytes, 4);
    if code != 0 {
        return ParcelString::Exception {
            code,
            message: match string {
                ParcelString::Value(value) => value,
                ParcelString::Exception { .. } | ParcelString::Invalid { .. } => None,
            },
        };
    }
    string
}

fn invalid(reason: &str) -> ParcelString {
    ParcelString::Invalid {
        reason: reason.to_owned(),
    }
}

/// The line without an offset such as `0x00000010:` and the spaces after it, as
/// `/^\s*0x[0-9a-fA-F]+:\s*/` removes it.
fn without_offset(line: &str) -> &str {
    let rest = text::trim_start(line);
    let Some(hex) = rest.strip_prefix("0x") else {
        return line;
    };
    let digits = hex
        .find(|c: char| !c.is_ascii_hexdigit())
        .unwrap_or(hex.len());
    if digits == 0 {
        return line;
    }
    match hex[digits..].strip_prefix(':') {
        Some(after) => text::trim_start(after),
        None => line,
    }
}

/// The words at the start of a line, as `/^(?:[0-9a-fA-F]{8}(?: +|$))+/` matches them: groups of
/// eight hex digits, each followed by spaces or the end of the line.
fn hex_words(line: &str) -> Vec<u32> {
    let mut words = Vec::new();
    let mut rest = line;
    while let Some(word) = rest.get(..8)
        && word.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        let after = &rest[8..];
        let spaced = after.trim_start_matches(' ');
        if !after.is_empty() && spaced.len() == after.len() {
            break;
        }
        words.push(u32::from_str_radix(word, 16).expect("eight hex digits fit in a u32"));
        rest = spaced;
    }
    words
}

fn read_i32(bytes: &[u8], offset: usize) -> Option<i32> {
    let word = bytes.get(offset..offset.checked_add(4)?)?;
    Some(i32::from_le_bytes(word.try_into().ok()?))
}

/// The `String16` at `offset`: a [`ParcelString::Value`] or a [`ParcelString::Invalid`].
fn read_string16(bytes: &[u8], offset: usize) -> ParcelString {
    let Some(length) = read_i32(bytes, offset) else {
        return invalid("Parcel ends before the string length");
    };
    if length == -1 {
        return ParcelString::Value(None);
    }
    let units = usize::try_from(length)
        .ok()
        .and_then(|units| bytes.get(offset + 4..offset + 4 + units * 2));
    let Some(units) = units else {
        return invalid(&format!("Parcel string length {length} exceeds the reply"));
    };
    let units: Vec<u16> = units
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&unit| u16::from_le_bytes(unit))
        .collect();
    ParcelString::Value(Some(String::from_utf16_lossy(&units)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(text: &str) -> ParcelString {
        ParcelString::Value(Some(text.to_owned()))
    }

    #[test]
    fn a_null_string_on_one_line() {
        assert_eq!(
            parse_parcel_string("Result: Parcel(\t00000000 ffffffff   '........')"),
            ParcelString::Value(None)
        );
    }

    #[test]
    fn a_binder_exception_with_a_message() {
        assert_eq!(
            parse_parcel_string(
                "Result: Parcel(\n0x00000000: ffffffff 00000003 00690068 00000021 '....h.i.!...')"
            ),
            ParcelString::Exception {
                code: -1,
                message: Some("hi!".to_owned()),
            }
        );
    }

    #[test]
    fn an_exception_without_a_readable_message() {
        assert_eq!(
            parse_parcel_string("Result: Parcel(fffffffc '....')"),
            ParcelString::Exception {
                code: -4,
                message: None,
            }
        );
    }

    #[test]
    fn a_length_longer_than_the_reply_is_invalid() {
        assert_eq!(
            parse_parcel_string("Result: Parcel(00000000 00000010 00310031 '....')"),
            invalid("Parcel string length 16 exceeds the reply")
        );
        assert_eq!(
            parse_parcel_string("Result: Parcel(00000000 fffffffe '....')"),
            invalid("Parcel string length -2 exceeds the reply")
        );
    }

    #[test]
    fn a_reply_without_a_string() {
        assert_eq!(
            parse_parcel_string("Result: Parcel(00000000 '....')"),
            invalid("Parcel ends before the string length")
        );
        assert_eq!(
            parse_parcel_string("Result: Parcel()"),
            invalid("empty Parcel")
        );
    }

    #[test]
    fn output_without_a_parcel() {
        assert_eq!(parse_parcel_string("  \n"), invalid("no Parcel in output"));
        assert_eq!(
            parse_parcel_string("\n  first line \nsecond"),
            invalid("first line")
        );
    }

    #[test]
    fn words_stop_at_the_ascii_column_and_at_a_run_longer_than_a_word() {
        assert_eq!(
            hex_words("00000000 0000000f 00350033 '...'"),
            [0, 15, 0x0035_0033]
        );
        assert_eq!(hex_words("00000000 000000001"), [0]);
        assert_eq!(hex_words("00000000"), [0]);
        assert_eq!(hex_words("0000000"), Vec::<u32>::new());
    }

    #[test]
    fn the_dump_ends_at_the_closing_quote_and_parenthesis() {
        let output = "Result: Parcel(\n\
                      0x00000000: 00000000 00000001 00000041 '........A...')\n\
                      0x00000010: 00000000 00000000 '........')";
        assert_eq!(parse_parcel_string(output), value("A"));
    }

    #[test]
    fn a_lone_surrogate_reads_as_a_replacement_character() {
        assert_eq!(
            parse_parcel_string("Result: Parcel(00000000 00000001 0000d800 '....')"),
            value("\u{fffd}")
        );
    }
}
