//! The rows `content query` prints.

use std::collections::BTreeMap;

use thiserror::Error;

use crate::text;

/// One row of `content query` output: each projected column's value, `None` for SQL `NULL`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContentRow {
    values: BTreeMap<String, Option<String>>,
}

impl ContentRow {
    /// The value of `column`, or `None` when it is `NULL` or the row has no such column.
    #[must_use]
    pub fn get(&self, column: &str) -> Option<&str> {
        self.values.get(column).and_then(Option::as_deref)
    }

    /// The value of `column` as an integer, when it is one exactly (`/^-?\d+$/`).
    #[must_use]
    pub fn integer(&self, column: &str) -> Option<i64> {
        self.get(column).and_then(text::parse_int)
    }

    /// How the value of `column` reads in an error message, as JavaScript printed it: the text,
    /// `null` for `NULL`, or `undefined` for a column the row doesn't have.
    pub(crate) fn describe(&self, column: &str) -> String {
        match self.values.get(column) {
            Some(Some(value)) => value.clone(),
            Some(None) => "null".to_owned(),
            None => "undefined".to_owned(),
        }
    }
}

/// The rows of one `content query`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContentRows {
    /// The rows that matched the projection, in order.
    pub rows: Vec<ContentRow>,
    /// The row lines that didn't, kept for the log.
    pub malformed: Vec<String>,
}

/// Parses `content query` output: one `Row: N col=value, col=value` line per row, `NULL` for SQL
/// null, and `No result found.` when there are none.
///
/// The tool doesn't escape values, so a value holding `, ` would be ambiguous; each value
/// therefore runs up to the next expected `, <column>=` of the projection, in order. A row
/// missing a column, or with its columns out of order, is malformed. The output's last newline
/// ends the last row, and any other line that doesn't start a row, even an empty one, continues
/// the value before it. Text that can hold arbitrary characters, such as bodies and file names,
/// is selected as `hex(...)`, which leaves only digits and A to F.
#[must_use]
pub fn parse_content_rows(output: &str, columns: &[&str]) -> ContentRows {
    let text = output.replace("\r\n", "\n");
    let text = text.strip_suffix('\n').unwrap_or(&text);
    let mut lines: Vec<String> = Vec::new();
    for line in text.split('\n') {
        if row_body(line).is_some() {
            lines.push(line.to_owned());
        } else if let Some(last) = lines.last_mut() {
            last.push('\n');
            last.push_str(line);
        }
    }
    let mut rows = ContentRows::default();
    for line in lines {
        match row_body(&line).and_then(|body| parse_row(body, columns)) {
            Some(row) => rows.rows.push(row),
            None => rows.malformed.push(line),
        }
    }
    rows
}

/// The line after its `Row: N ` prefix, when it has one.
fn row_body(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("Row: ")?;
    let digits = text::leading_digits(rest);
    if digits.is_empty() {
        return None;
    }
    rest[digits.len()..].strip_prefix(' ')
}

fn parse_row(text: &str, columns: &[&str]) -> Option<ContentRow> {
    let mut row = ContentRow::default();
    let mut position = 0;
    for (index, column) in columns.iter().enumerate() {
        let start = position + column.len() + 1;
        if !text[position..].starts_with(column) || text.get(start - 1..start) != Some("=") {
            return None;
        }
        let end = match columns.get(index + 1) {
            Some(next) => start + text[start..].find(&format!(", {next}="))?,
            None => text.len(),
        };
        let value = &text[start..end];
        row.values.insert(
            (*column).to_owned(),
            (value != "NULL").then(|| value.to_owned()),
        );
        position = (end + 2).min(text.len());
    }
    Some(row)
}

/// Hex text with an odd length or a character other than a hex digit.
#[must_use]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
#[error("hex text has an odd length or a non-hex character")]
pub struct HexError;

/// Decodes SQLite's `hex()` of a `TEXT` value, which is its UTF-8 bytes; `hex(NULL)` is empty. The
/// bytes decode as JavaScript's `TextDecoder` does: an invalid sequence becomes U+FFFD, and a
/// leading byte order mark is dropped.
///
/// # Errors
///
/// [`HexError`] when the text has an odd length or a character other than a hex digit.
pub fn decode_hex(hex: Option<&str>) -> Result<String, HexError> {
    let hex = hex.unwrap_or("");
    if !hex.len().is_multiple_of(2) || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(HexError);
    }
    let bytes: Vec<u8> = hex
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&[high, low]| hex_value(high) << 4 | hex_value(low))
        .collect();
    let text = String::from_utf8_lossy(&bytes);
    Ok(text.strip_prefix('\u{feff}').unwrap_or(&text).to_owned())
}

/// The value of one ASCII hex digit.
fn hex_value(digit: u8) -> u8 {
    match digit {
        b'0'..=b'9' => digit - b'0',
        b'a'..=b'f' => digit - b'a' + 10,
        _ => digit - b'A' + 10,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(columns: &[&str], values: &[Option<&str>]) -> ContentRow {
        ContentRow {
            values: columns
                .iter()
                .zip(values)
                .map(|(column, value)| ((*column).to_owned(), value.map(str::to_owned)))
                .collect(),
        }
    }

    #[test]
    fn empty_results_and_nulls() {
        assert_eq!(
            parse_content_rows("No result found.\n", &["_id"]),
            ContentRows::default()
        );
        let rows = parse_content_rows(
            "Row: 0 _id=1, mid=1000000007, ct=NULL, hex(text)=\n",
            &["_id", "mid", "ct", "hex(text)"],
        );
        assert_eq!(
            rows.rows,
            [row(
                &["_id", "mid", "ct", "hex(text)"],
                &[Some("1"), Some("1000000007"), None, Some("")]
            )]
        );
    }

    #[test]
    fn values_with_commas_equals_signs_and_newlines_stay_whole() {
        let output = "Row: 0 _id=12, address=Smith, J=x, date=5, hex(body)=4869\n\
                      Row: 1 _id=13, address=line one\n\
                      \n\
                      line two, date=6, hex(body)=\n";
        let rows = parse_content_rows(output, &["_id", "address", "date", "hex(body)"]);
        assert_eq!(rows.malformed, Vec::<String>::new());
        assert_eq!(
            rows.rows,
            [
                row(
                    &["_id", "address", "date", "hex(body)"],
                    &[Some("12"), Some("Smith, J=x"), Some("5"), Some("4869")]
                ),
                row(
                    &["_id", "address", "date", "hex(body)"],
                    &[
                        Some("13"),
                        Some("line one\n\nline two"),
                        Some("6"),
                        Some("")
                    ]
                ),
            ]
        );
    }

    #[test]
    fn a_row_missing_a_column_or_out_of_order_is_malformed() {
        let rows = parse_content_rows(
            "Row: 0 _id=1, date=2\nRow: 1 _id=2\nRow: 2 date=3, _id=4\n",
            &["_id", "date"],
        );
        assert_eq!(rows.rows, [row(&["_id", "date"], &[Some("1"), Some("2")])]);
        assert_eq!(rows.malformed, ["Row: 1 _id=2", "Row: 2 date=3, _id=4"]);
    }

    #[test]
    fn row_prefixes() {
        assert_eq!(row_body("Row: 12 x"), Some("x"));
        assert_eq!(row_body("Row: x"), None);
        assert_eq!(row_body("Row: 1"), None);
        assert_eq!(row_body("row: 1 x"), None);
    }

    #[test]
    fn carriage_returns_before_newlines_are_dropped() {
        let rows = parse_content_rows("Row: 0 _id=1\r\nRow: 1 _id=2\r\n", &["_id"]);
        assert_eq!(rows.rows.len(), 2);
        assert_eq!(rows.rows[0].get("_id"), Some("1"));
    }

    #[test]
    fn values_read_as_integers_only_when_exact() {
        let row = row(&["a", "b", "c"], &[Some("-12"), Some("1.5"), None]);
        assert_eq!(row.integer("a"), Some(-12));
        assert_eq!(row.integer("b"), None);
        assert_eq!(row.integer("c"), None);
        assert_eq!(row.describe("c"), "null");
        assert_eq!(row.describe("d"), "undefined");
    }

    #[test]
    fn hex_decodes_as_utf8() {
        assert_eq!(
            decode_hex(Some("F09F918B2C20686921")),
            Ok("👋, hi!".to_owned())
        );
        assert_eq!(decode_hex(Some("")), Ok(String::new()));
        assert_eq!(decode_hex(None), Ok(String::new()));
        assert_eq!(decode_hex(Some("ABC")), Err(HexError));
        assert_eq!(decode_hex(Some("ZZ")), Err(HexError));
        assert_eq!(decode_hex(Some("EFBBBF41")), Ok("A".to_owned()));
        assert_eq!(decode_hex(Some("41FF42")), Ok("A\u{fffd}B".to_owned()));
    }
}
