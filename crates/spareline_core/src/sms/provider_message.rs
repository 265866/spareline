//! Inbound messages as the phone's providers hold them.

use thiserror::Error;

use crate::Timestamp;
use crate::sms::{ContentRow, HexError, MmsPart, Provider, content_rows};

/// `PduHeaders.FROM` in AOSP: the address type of an MMS's sender.
const ADDR_FROM: &str = "137";

/// The latest time a [`Timestamp`] can write: the end of the year 9999.
const MAX_MILLIS: i64 = 253_402_300_799_999;

/// One inbound message as the phone's provider holds it, ready to store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderMessage {
    /// The provider it came from.
    pub provider: Provider,
    /// The provider's `_id`.
    pub provider_id: i64,
    /// The subscription it arrived on.
    pub sub_id: Option<i64>,
    /// The sender's address, as the provider spells it.
    pub sender: Option<String>,
    /// The text: an SMS's body, or an MMS's `text/plain` parts.
    pub body: String,
    /// An MMS's subject; `None` when empty, and always for an SMS.
    pub subject: Option<String>,
    /// When the sender sent it, when the provider knows.
    pub sent_at: Option<Timestamp>,
    /// When the phone received it.
    pub received_at: Timestamp,
    /// The binary MMS parts still to be read from the phone, in `(seq, partId)` order.
    pub parts: Vec<PartRef>,
}

/// A binary MMS part to read with [`read_part_command`](crate::read_part_command).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartRef {
    /// The part's `_id`.
    pub part_id: i64,
    /// Its content type, in lower case.
    pub content_type: String,
    /// Its file name, when it has one.
    pub filename: Option<String>,
}

/// A provider row that can't become a message.
#[must_use]
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum ProviderRowError {
    /// A required column isn't an integer.
    #[error("provider row has no integer {column}: {value}")]
    MissingInteger {
        /// The column.
        column: String,
        /// Its value as spareline-old printed it: the text, `null` or `undefined`.
        value: String,
    },
    /// An SMS whose date isn't after the epoch.
    #[error("SMS row has no date")]
    SmsWithoutDate,
    /// An MMS whose date isn't after the epoch.
    #[error("MMS row has no date")]
    MmsWithoutDate,
    /// A time after the end of the year 9999, which no timestamp column can hold.
    #[error("provider time {value} is after the year 9999")]
    TimeOutOfRange {
        /// The column's value: milliseconds for an SMS, seconds for an MMS.
        value: i64,
    },
    /// A `hex(...)` column that isn't hex.
    #[error(transparent)]
    Hex(#[from] HexError),
}

/// The required integer `column`.
pub(crate) fn required(row: &ContentRow, column: &str) -> Result<i64, ProviderRowError> {
    row.integer(column)
        .ok_or_else(|| ProviderRowError::MissingInteger {
            column: column.to_owned(),
            value: row.describe(column),
        })
}

/// The time `value` units of `scale` milliseconds after the epoch. `None` for a value that is
/// missing, or not after the epoch.
pub(crate) fn provider_time(
    value: Option<i64>,
    scale: i64,
) -> Result<Option<Timestamp>, ProviderRowError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let millis = value.checked_mul(scale);
    match millis {
        Some(millis) if millis <= 0 => Ok(None),
        Some(millis) if millis <= MAX_MILLIS => Ok(Some(Timestamp::from_millis(millis))),
        _ => Err(ProviderRowError::TimeOutOfRange { value }),
    }
}

/// An optional time such as `date_sent`: a value out of range reads as no time, so a garbled
/// sent time never costs the text, whose required received time decides whether it is stored.
fn optional_time(value: Option<i64>, scale: i64) -> Option<Timestamp> {
    provider_time(value, scale).ok().flatten()
}

/// The message an SMS inbox row (of [`SMS_COLUMNS`](crate::SMS_COLUMNS)) holds. SMS dates are
/// milliseconds after the epoch; a `date_sent` of zero or less, or not an integer, is no sent
/// time.
///
/// # Errors
///
/// [`ProviderRowError`] when `date` or `_id` isn't an integer, `date` isn't after the epoch, a
/// time is after the year 9999, or the body isn't hex.
pub fn sms_message(row: &ContentRow) -> Result<ProviderMessage, ProviderRowError> {
    let received_at =
        provider_time(Some(required(row, "date")?), 1)?.ok_or(ProviderRowError::SmsWithoutDate)?;
    let provider_id = required(row, "_id")?;
    let body = content_rows::decode_hex(row.get("hex(body)"))?;
    Ok(ProviderMessage {
        provider: Provider::Sms,
        provider_id,
        sub_id: row.integer("sub_id"),
        sender: row.get("address").map(str::to_owned),
        body,
        subject: None,
        sent_at: optional_time(row.integer("date_sent"), 1),
        received_at,
        parts: Vec::new(),
    })
}

/// The message an MMS inbox row (of [`MMS_COLUMNS`](crate::MMS_COLUMNS)) holds, with its address
/// rows and its parts. MMS dates are seconds after the epoch.
///
/// The sender is the first address of type 137 (`FROM`). The body is the non-empty `text/plain`
/// parts joined with `\n`, in `(seq, partId)` order; SMIL, the slide layout, is dropped; every
/// other part becomes a [`PartRef`] to read.
///
/// # Errors
///
/// [`ProviderRowError`] when `date` or `_id` isn't an integer, `date` isn't after the epoch, a
/// time is after the year 9999, or the subject isn't hex.
pub fn mms_message(
    row: &ContentRow,
    addresses: &[ContentRow],
    parts: &[MmsPart],
) -> Result<ProviderMessage, ProviderRowError> {
    let received_at = provider_time(Some(required(row, "date")?), 1000)?
        .ok_or(ProviderRowError::MmsWithoutDate)?;
    let sent_seconds = row.integer("date_sent");
    let subject = content_rows::decode_hex(row.get("hex(sub)"))?;
    let mut ordered: Vec<&MmsPart> = parts.iter().collect();
    ordered.sort_by_key(|part| (part.seq, part.part_id));
    let texts: Vec<&str> = ordered
        .iter()
        .filter(|part| part.content_type == "text/plain" && !part.text.is_empty())
        .map(|part| part.text.as_str())
        .collect();
    Ok(ProviderMessage {
        provider: Provider::Mms,
        provider_id: required(row, "_id")?,
        sub_id: row.integer("sub_id"),
        sender: addresses
            .iter()
            .find(|address| address.get("type") == Some(ADDR_FROM))
            .and_then(|address| address.get("address"))
            .map(str::to_owned),
        body: texts.join("\n"),
        subject: (!subject.is_empty()).then_some(subject),
        sent_at: optional_time(sent_seconds, 1000),
        received_at,
        parts: ordered
            .iter()
            .filter(|part| {
                part.content_type != "text/plain" && part.content_type != "application/smil"
            })
            .map(|part| PartRef {
                part_id: part.part_id,
                content_type: part.content_type.clone(),
                filename: part.filename.clone(),
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sms::{self, MMS_ADDR_COLUMNS, MMS_COLUMNS, MMS_PART_COLUMNS, SMS_COLUMNS};

    fn rows(output: &str, columns: &[&str]) -> Vec<ContentRow> {
        let parsed = content_rows::parse_content_rows(output, columns);
        assert_eq!(parsed.malformed, Vec::<String>::new(), "every row parses");
        parsed.rows
    }

    fn sms(fields: &str) -> Result<ProviderMessage, ProviderRowError> {
        sms_message(&rows(&format!("Row: 0 {fields}"), SMS_COLUMNS)[0])
    }

    #[test]
    fn an_sms_row() {
        let message = sms(
            "_id=4, thread_id=2, address=550, date=1792637380314, date_sent=1792623882000, \
             sub_id=1, read=1, type=1, hex(body)=4869",
        )
        .expect("a message");
        assert_eq!(
            message,
            ProviderMessage {
                provider: Provider::Sms,
                provider_id: 4,
                sub_id: Some(1),
                sender: Some("550".to_owned()),
                body: "Hi".to_owned(),
                subject: None,
                sent_at: Some(Timestamp::from_millis(1_792_623_882_000)),
                received_at: Timestamp::from_millis(1_792_637_380_314),
                parts: Vec::new(),
            }
        );
    }

    #[test]
    fn a_garbled_sent_time_keeps_the_text() {
        let message = sms(
            "_id=4, thread_id=2, address=550, date=1792637380314, date_sent=253402300800000, \
             sub_id=1, read=1, type=1, hex(body)=4869",
        )
        .expect("the text is stored without a sent time");
        assert_eq!(message.sent_at, None);
        assert_eq!(message.body, "Hi");
    }

    #[test]
    fn sms_rows_that_cannot_become_messages() {
        let base = |date: &str, id: &str, body: &str| {
            sms(&format!(
                "_id={id}, thread_id=1, address=NULL, date={date}, date_sent=0, sub_id=NULL, \
                 read=1, type=1, hex(body)={body}"
            ))
        };
        assert_eq!(base("0", "1", ""), Err(ProviderRowError::SmsWithoutDate));
        assert_eq!(
            base("x", "1", ""),
            Err(ProviderRowError::MissingInteger {
                column: "date".to_owned(),
                value: "x".to_owned(),
            })
        );
        assert_eq!(
            base("NULL", "1", "").map_err(|error| error.to_string()),
            Err("provider row has no integer date: null".to_owned())
        );
        assert_eq!(
            base("5", "NULL", "").map_err(|error| error.to_string()),
            Err("provider row has no integer _id: null".to_owned())
        );
        assert_eq!(base("5", "1", "ABC"), Err(ProviderRowError::Hex(HexError)));
        assert_eq!(
            base("253402300800000", "1", ""),
            Err(ProviderRowError::TimeOutOfRange {
                value: 253_402_300_800_000,
            })
        );
        let message = base("5", "1", "").expect("a message");
        assert_eq!(message.sender, None);
        assert_eq!(message.sub_id, None);
        assert_eq!(message.sent_at, None);
    }

    fn part(fields: &str) -> MmsPart {
        sms::mms_part(&rows(&format!("Row: 0 {fields}"), MMS_PART_COLUMNS)[0]).expect("a part")
    }

    #[test]
    fn mms_parts() {
        let image = part(
            "_id=12, mid=3, seq=x, ct=IMAGE/JPEG, hex(name)=, hex(fn)=612E6A7067, hex(cl)=62, \
             hex(text)=",
        );
        assert_eq!(
            image,
            MmsPart {
                part_id: 12,
                mid: 3,
                seq: 0,
                content_type: "image/jpeg".to_owned(),
                filename: Some("a.jpg".to_owned()),
                text: String::new(),
            }
        );
        let unnamed =
            part("_id=1, mid=3, seq=0, ct=NULL, hex(name)=, hex(fn)=, hex(cl)=, hex(text)=");
        assert_eq!(unnamed.content_type, "application/octet-stream");
        assert_eq!(unnamed.filename, None);
        let bad = sms::mms_part(
            &rows(
                "Row: 0 _id=NULL, mid=3, seq=0, ct=NULL, hex(name)=, hex(fn)=, hex(cl)=, hex(text)=",
                MMS_PART_COLUMNS,
            )[0],
        );
        assert!(
            matches!(bad, Err(ProviderRowError::MissingInteger { column, .. }) if column == "_id"),
            "a part needs an integer _id"
        );
    }

    #[test]
    fn an_mms_with_text_smil_and_an_image() {
        let row = &rows(
            "Row: 0 _id=3, date=1792637380, date_sent=1792637370, sub_id=1, m_type=132, \
             hex(sub)=5375626A",
            MMS_COLUMNS,
        )[0];
        let addresses = rows(
            "Row: 0 type=151, address=+15125550100\nRow: 1 type=137, address=+15125550151\n\
             Row: 2 type=137, address=+15125550152\n",
            MMS_ADDR_COLUMNS,
        );
        let parts = [
            part(
                "_id=20, mid=3, seq=1, ct=text/plain, hex(name)=, hex(fn)=, hex(cl)=, hex(text)=7365636F6E64",
            ),
            part(
                "_id=19, mid=3, seq=1, ct=text/plain, hex(name)=, hex(fn)=, hex(cl)=, hex(text)=6669727374",
            ),
            part(
                "_id=18, mid=3, seq=0, ct=application/smil, hex(name)=, hex(fn)=, hex(cl)=, hex(text)=3C736D696C3E",
            ),
            part("_id=21, mid=3, seq=2, ct=text/plain, hex(name)=, hex(fn)=, hex(cl)=, hex(text)="),
            part(
                "_id=17, mid=3, seq=5, ct=image/png, hex(name)=, hex(fn)=, hex(cl)=702E706E67, hex(text)=",
            ),
        ];
        let message = mms_message(row, &addresses, &parts).expect("a message");
        assert_eq!(
            message,
            ProviderMessage {
                provider: Provider::Mms,
                provider_id: 3,
                sub_id: Some(1),
                sender: Some("+15125550151".to_owned()),
                body: "first\nsecond".to_owned(),
                subject: Some("Subj".to_owned()),
                sent_at: Some(Timestamp::from_millis(1_792_637_370_000)),
                received_at: Timestamp::from_millis(1_792_637_380_000),
                parts: vec![PartRef {
                    part_id: 17,
                    content_type: "image/png".to_owned(),
                    filename: Some("p.png".to_owned()),
                }],
            }
        );
    }

    #[test]
    fn an_mms_without_a_sender_subject_or_date() {
        let row = |date: &str| {
            rows(
                &format!(
                    "Row: 0 _id=3, date={date}, date_sent=NULL, sub_id=NULL, m_type=132, hex(sub)="
                ),
                MMS_COLUMNS,
            )
            .remove(0)
        };
        let message = mms_message(&row("1"), &[], &[]).expect("a message");
        assert_eq!(message.sender, None);
        assert_eq!(message.subject, None);
        assert_eq!(message.body, "");
        assert_eq!(message.sent_at, None);
        assert_eq!(
            mms_message(&row("-1"), &[], &[]),
            Err(ProviderRowError::MmsWithoutDate)
        );
        assert_eq!(
            mms_message(&row("9223372036854775807"), &[], &[]),
            Err(ProviderRowError::TimeOutOfRange { value: i64::MAX })
        );
    }
}
