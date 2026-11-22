//! Whether a `content` command's output can be trusted.

use thiserror::Error;

use crate::sms::{ContentRows, content_rows};
use crate::text;

/// A `content query` that failed.
#[must_use]
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum ContentQueryError {
    /// A non-zero exit, or an exception on stderr.
    #[error("content query failed (exit {exit_code}): {stderr}")]
    Failed {
        /// The exit status.
        exit_code: i32,
        /// The first line of the trimmed stderr.
        stderr: String,
    },
    /// Output that is neither rows nor `No result found.`.
    #[error("unexpected content query output: {start}")]
    Unexpected {
        /// The first 120 UTF-16 code units of the trimmed stdout, or `(empty)`.
        start: String,
    },
}

/// Checks the result of a `content query` and parses its rows. `content` reports provider errors
/// on stderr, sometimes with exit status 0, so stderr that mentions `Exception` or `Error while`,
/// in any case, fails too. Stdout must be `No result found.` or start with `Row: `.
///
/// # Errors
///
/// [`ContentQueryError`] for a non-zero exit, an error on stderr, or output that isn't rows.
pub fn check_content_output(
    stdout: &str,
    stderr: &str,
    exit_code: i32,
    columns: &[&str],
) -> Result<ContentRows, ContentQueryError> {
    let stderr = text::trim(stderr);
    let lower = stderr.to_ascii_lowercase();
    if exit_code != 0 || lower.contains("exception") || lower.contains("error while") {
        return Err(ContentQueryError::Failed {
            exit_code,
            stderr: stderr.split('\n').next().unwrap_or("").to_owned(),
        });
    }
    let trimmed = text::trim(stdout);
    if trimmed != "No result found." && !trimmed.starts_with("Row: ") {
        let start = utf16_prefix(trimmed, 120);
        return Err(ContentQueryError::Unexpected {
            start: if start.is_empty() { "(empty)" } else { start }.to_owned(),
        });
    }
    Ok(content_rows::parse_content_rows(stdout, columns))
}

/// A `content read` of an MMS part that gave no bytes to keep.
#[must_use]
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum PartReadError {
    /// A non-zero exit.
    #[error("exit {exit_code}")]
    Exit {
        /// The exit status.
        exit_code: i32,
    },
    /// Anything on stderr: the tool prints provider errors there and still exits 0.
    #[error("stderr: {line}")]
    Stderr {
        /// The first line of stderr, up to 120 UTF-16 code units.
        line: String,
    },
    /// No bytes.
    #[error("empty")]
    Empty,
}

/// Checks the result of [`read_part_command`](crate::read_part_command): it must exit 0, print
/// nothing on stderr, and print some bytes.
///
/// # Errors
///
/// [`PartReadError`] saying which of those failed, for the log.
pub fn check_part_read(exit_code: i32, stdout: &[u8], stderr: &[u8]) -> Result<(), PartReadError> {
    if exit_code != 0 {
        return Err(PartReadError::Exit { exit_code });
    }
    if !stderr.is_empty() {
        let decoded = String::from_utf8_lossy(stderr);
        let decoded = decoded.strip_prefix('\u{feff}').unwrap_or(&decoded);
        let line = decoded.split('\n').next().unwrap_or("");
        return Err(PartReadError::Stderr {
            line: utf16_prefix(line, 120).to_owned(),
        });
    }
    if stdout.is_empty() {
        return Err(PartReadError::Empty);
    }
    Ok(())
}

/// The longest start of `text` of at most `units` UTF-16 code units that ends on a character.
fn utf16_prefix(text: &str, units: usize) -> &str {
    let mut used = 0;
    for (index, c) in text.char_indices() {
        used += c.len_utf16();
        if used > units {
            return &text[..index];
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_query() {
        assert_eq!(
            check_content_output("", "  boom\nsecond", 1, &["_id"]),
            Err(ContentQueryError::Failed {
                exit_code: 1,
                stderr: "boom".to_owned(),
            })
        );
        let error = check_content_output(
            "Row: 0 _id=1",
            "java.lang.IllegalArgumentEXCEPTION: x",
            0,
            &["_id"],
        )
        .expect_err("an exception on stderr fails");
        assert_eq!(
            error.to_string(),
            "content query failed (exit 0): java.lang.IllegalArgumentEXCEPTION: x"
        );
        assert!(
            check_content_output("Row: 0 _id=1", "Error While reading", 0, &["_id"]).is_err(),
            "an error on stderr fails"
        );
    }

    #[test]
    fn unexpected_output() {
        assert_eq!(
            check_content_output(" \n", "", 0, &["_id"]),
            Err(ContentQueryError::Unexpected {
                start: "(empty)".to_owned(),
            })
        );
        let long = "x".repeat(200);
        assert_eq!(
            check_content_output(&long, "", 0, &["_id"]),
            Err(ContentQueryError::Unexpected {
                start: "x".repeat(120),
            })
        );
    }

    #[test]
    fn rows_and_empty_results_pass_with_harmless_stderr() {
        let rows = check_content_output("Row: 0 _id=1\n", "warning: slow", 0, &["_id"])
            .expect("rows pass");
        assert_eq!(rows.rows.len(), 1);
        let empty = check_content_output("No result found.\n", "", 0, &["_id"]).expect("passes");
        assert_eq!(empty, ContentRows::default());
    }

    #[test]
    fn part_reads() {
        assert_eq!(check_part_read(0, b"bytes", b""), Ok(()));
        assert_eq!(
            check_part_read(2, b"bytes", b""),
            Err(PartReadError::Exit { exit_code: 2 })
        );
        assert_eq!(
            check_part_read(0, b"bytes", b"No such file\nmore"),
            Err(PartReadError::Stderr {
                line: "No such file".to_owned(),
            })
        );
        assert_eq!(check_part_read(0, b"", b""), Err(PartReadError::Empty));
        assert_eq!(PartReadError::Empty.to_string(), "empty");
    }

    #[test]
    fn utf16_prefixes_end_on_a_character() {
        assert_eq!(utf16_prefix("ab👋c", 3), "ab");
        assert_eq!(utf16_prefix("ab👋c", 4), "ab👋");
        assert_eq!(utf16_prefix("abc", 10), "abc");
    }
}
