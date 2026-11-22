//! MMS part rows.

use crate::sms::provider_message::{self, ProviderRowError};
use crate::sms::{ContentRow, content_rows};

/// One MMS part row (of [`MMS_PART_COLUMNS`](crate::MMS_PART_COLUMNS)).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MmsPart {
    /// The part's `_id`.
    pub part_id: i64,
    /// The `_id` of the MMS it belongs to.
    pub mid: i64,
    /// Its place in the message; 0 when not an integer.
    pub seq: i64,
    /// Its content type in lower case; `application/octet-stream` when `NULL`.
    pub content_type: String,
    /// Its name, else its file name, else its content location; `None` when all are empty.
    pub filename: Option<String>,
    /// Its text, for a text part.
    pub text: String,
}

/// The part an MMS part row holds.
///
/// # Errors
///
/// [`ProviderRowError`] when a `hex(...)` column isn't hex, or `_id` or `mid` isn't an integer.
pub fn mms_part(row: &ContentRow) -> Result<MmsPart, ProviderRowError> {
    let name = content_rows::decode_hex(row.get("hex(name)"))?;
    let file = content_rows::decode_hex(row.get("hex(fn)"))?;
    let location = content_rows::decode_hex(row.get("hex(cl)"))?;
    let part_id = provider_message::required(row, "_id")?;
    let mid = provider_message::required(row, "mid")?;
    Ok(MmsPart {
        part_id,
        mid,
        seq: row.integer("seq").unwrap_or(0),
        content_type: row
            .get("ct")
            .unwrap_or("application/octet-stream")
            .to_lowercase(),
        filename: [name, file, location]
            .into_iter()
            .find(|candidate| !candidate.is_empty()),
        text: content_rows::decode_hex(row.get("hex(text)"))?,
    })
}

/// What two reads of a downloading MMS must agree on before it is stored: the row's date, sub id
/// and message type, and each part's id, content type and text length. AOSP's download code
/// inserts the row and then updates its date and subscription, so one read can catch a
/// half-written MMS.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct MmsSignature {
    /// The row's `date`, as printed.
    pub date: Option<String>,
    /// The row's `sub_id`, as printed.
    pub sub_id: Option<String>,
    /// The row's `m_type`, as printed.
    pub message_type: Option<String>,
    /// The message's parts, in the order given.
    pub parts: Vec<MmsPartSignature>,
}

/// One part of an [`MmsSignature`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct MmsPartSignature {
    /// The part's `_id`.
    pub part_id: i64,
    /// Its content type.
    pub content_type: String,
    /// Its text's length in UTF-16 code units, as JavaScript counted it.
    pub text_length: usize,
}

/// The signature of an MMS row and its parts.
#[must_use]
pub fn mms_signature(row: &ContentRow, parts: &[MmsPart]) -> MmsSignature {
    MmsSignature {
        date: row.get("date").map(str::to_owned),
        sub_id: row.get("sub_id").map(str::to_owned),
        message_type: row.get("m_type").map(str::to_owned),
        parts: parts
            .iter()
            .map(|part| MmsPartSignature {
                part_id: part.part_id,
                content_type: part.content_type.clone(),
                text_length: part.text.encode_utf16().count(),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sms::{MMS_COLUMNS, MMS_PART_COLUMNS, content_rows};

    #[test]
    fn signatures_change_with_the_row_and_the_parts() {
        let row = |date: &str| {
            content_rows::parse_content_rows(
                &format!("Row: 0 _id=3, date={date}, date_sent=0, sub_id=1, m_type=132, hex(sub)="),
                MMS_COLUMNS,
            )
            .rows
            .remove(0)
        };
        let part = |text: &str| {
            let rows = content_rows::parse_content_rows(
                &format!(
                    "Row: 0 _id=9, mid=3, seq=0, ct=text/plain, hex(name)=, hex(fn)=, hex(cl)=, hex(text)={text}"
                ),
                MMS_PART_COLUMNS,
            );
            mms_part(&rows.rows[0]).expect("a part")
        };
        let first = mms_signature(&row("5"), &[part("F09F918B")]);
        assert_eq!(first, mms_signature(&row("5"), &[part("F09F918B")]));
        assert_eq!(
            first.parts[0].text_length, 2,
            "one emoji is two UTF-16 units"
        );
        assert_ne!(first, mms_signature(&row("6"), &[part("F09F918B")]));
        assert_ne!(first, mms_signature(&row("5"), &[part("41")]));
        assert_ne!(first, mms_signature(&row("5"), &[]));
    }
}
