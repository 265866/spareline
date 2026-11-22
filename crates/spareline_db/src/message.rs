//! Texts as the database stores them and the API serves them.

use rusqlite::ToSql;
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSqlOutput, ValueRef};
use serde::Serialize;
use spareline_core::Timestamp;

/// Which of the phone's providers a text came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    /// The SMS provider.
    Sms,
    /// The MMS provider.
    Mms,
}

impl Provider {
    /// The provider as the database and the API spell it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sms => "sms",
            Self::Mms => "mms",
        }
    }
}

impl FromSql for Provider {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        match value.as_str()? {
            "sms" => Ok(Self::Sms),
            "mms" => Ok(Self::Mms),
            other => Err(FromSqlError::Other(
                format!("unknown provider {other:?}").into(),
            )),
        }
    }
}

impl ToSql for Provider {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::from(self.as_str()))
    }
}

/// An inbound text read from a phone's provider, ready to store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewMessage {
    /// The provider it came from.
    pub provider: Provider,
    /// The provider row's `_id`, which can repeat after Messages' data is cleared.
    pub provider_id: i64,
    /// Android's subscription id it arrived on.
    pub sub_id: Option<i64>,
    /// The sender's address.
    pub sender: Option<String>,
    /// The text.
    pub body: String,
    /// An MMS subject.
    pub subject: Option<String>,
    /// The carrier's time (`date_sent`).
    pub sent_at: Option<Timestamp>,
    /// The phone's time (`date`).
    pub received_at: Timestamp,
    /// Its MMS parts, already saved by [`AttachmentStore::save`](crate::AttachmentStore::save).
    pub attachments: Vec<NewAttachment>,
}

/// An MMS part saved under `DATA_DIR/attachments/<sha256>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewAttachment {
    /// The part's content type.
    pub content_type: String,
    /// Its size in bytes.
    pub size: i64,
    /// The lowercase hex SHA-256 of its bytes, which names its file.
    pub sha256: String,
    /// The file name the sender gave it.
    pub filename: Option<String>,
}

/// A text as the API serves it, the `Message` of surface.md section 3.1.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    /// The global cursor; it moves when the text is attributed to a number later.
    pub seq: i64,
    /// The provider it came from.
    pub provider: Provider,
    /// The provider row's `_id`.
    pub provider_id: i64,
    /// The phone's serial.
    pub phone: String,
    /// Android's subscription id.
    pub sub_id: Option<i64>,
    /// The SIM it arrived on, once known.
    pub sim_id: Option<i64>,
    /// The number it was sent to; null while its SIM is unknown.
    pub number: Option<MessageNumber>,
    /// The sender's address.
    pub from: Option<String>,
    /// The text.
    pub body: String,
    /// An MMS subject.
    pub subject: Option<String>,
    /// The carrier's time.
    pub sent_at: Option<String>,
    /// The phone's time.
    pub received_at: String,
    /// When spareline stored it.
    pub ingested_at: String,
    /// Its MMS parts, by id.
    pub attachments: Vec<AttachmentView>,
}

/// The number a text was sent to: `{"id", "e164", "note"}`, with `note` `""` when unset.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MessageNumber {
    /// The `numbers` row id.
    pub id: i64,
    /// The number in E.164, when known.
    pub e164: Option<String>,
    /// The owner's note.
    pub note: String,
}

/// An MMS part as a text lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentView {
    /// The `attachments` row id.
    pub id: i64,
    /// Its content type.
    pub content_type: String,
    /// Its size in bytes.
    pub size: i64,
    /// The file name the sender gave it.
    pub filename: Option<String>,
    /// Where to fetch it: `/api/v1/attachments/<id>`.
    pub url: String,
}

/// What `GET /api/v1/attachments/:id` needs to serve a part.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachmentFile {
    /// The SHA-256 that names its file.
    pub sha256: String,
    /// Its content type.
    pub content_type: String,
    /// The file name the sender gave it.
    pub filename: Option<String>,
}

/// A page of texts: `{"messages", "cursor"}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MessagePage {
    /// The texts.
    pub messages: Vec<Message>,
    /// Where to continue with `after`.
    pub cursor: i64,
}

/// Which texts a page draws from.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MessageScope {
    /// Only texts to this number.
    pub number_id: Option<i64>,
    /// Only texts matching this search, after trimming; an empty search is none. Surface.md
    /// section 3.4 says how it matches.
    pub search: Option<String>,
}

/// Which page of texts to read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Paging {
    /// Newest first, optionally with seqs below `before`. The cursor is the highest seq of all
    /// texts, read before the page.
    Newest {
        /// Only seqs below this.
        before: Option<i64>,
        /// At most this many.
        limit: i64,
    },
    /// Oldest first, with seqs above `after`. The cursor is the last seq returned, or `after`.
    After {
        /// Only seqs above this.
        after: i64,
        /// At most this many.
        limit: i64,
    },
}

/// A stored text's identity in its provider, which ingest reconciles against: the provider
/// row's `_id` and the phone's time, as stored.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct MessageKey {
    /// The provider row's `_id`.
    pub provider_id: i64,
    /// `received_at`, as stored.
    pub received_at: String,
}

/// The SIM and number a text belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Attribution {
    /// The SIM, when one is found.
    pub sim_id: Option<i64>,
    /// The number, when the SIM has one.
    pub number_id: Option<i64>,
}
