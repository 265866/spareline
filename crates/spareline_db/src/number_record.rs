//! A phone number as the database knows it.

use serde::Serialize;

use crate::field_patch::FieldPatch;
use crate::number_summary::{NumberSource, NumberSummary};
use crate::sim_summary::SimSummary;

/// A number with its SIM, where that SIM is, and its texts. The API's `PhoneNumber` adds two
/// fields the server owns from live state: `phoneStatus` (null when [`NumberRecord::phone`] is
/// null or removed, else the phone's live status or `offline`) and `radio`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NumberRecord {
    /// The row id.
    pub id: i64,
    /// The number in E.164, when known.
    pub e164: Option<String>,
    /// Where `e164` came from.
    pub source: NumberSource,
    /// The owner's note.
    pub note: String,
    /// What the SIM reports, from the open binding, else the latest one.
    #[serde(rename = "detectedE164")]
    pub detected_e164: Option<String>,
    /// Who typed the number, from the same binding.
    pub confirmed_by: Option<String>,
    /// When the number row was made.
    pub created_at: String,
    /// The SIM of the open binding, else the latest one.
    pub sim: Option<SimSummary>,
    /// Where that SIM is, or was last.
    pub phone: Option<NumberPhone>,
    /// How many texts it has.
    pub message_count: i64,
    /// Its newest text, by seq.
    pub last_message: Option<LastMessage>,
}

impl NumberRecord {
    /// The number as other records name it.
    #[must_use]
    pub fn summary(&self) -> NumberSummary {
        NumberSummary {
            id: self.id,
            e164: self.e164.clone(),
            source: self.source,
            note: self.note.clone(),
        }
    }
}

/// The phone a number's SIM is in, or was in last.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NumberPhone {
    /// The phone's serial.
    pub serial: String,
    /// Its model.
    pub model: Option<String>,
    /// The slot.
    pub slot: i64,
    /// When the SIM left this phone; null while it is in it.
    pub removed_at: Option<String>,
}

/// The newest text of a number.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LastMessage {
    /// Its seq.
    pub seq: i64,
    /// Its sender.
    pub from: Option<String>,
    /// The first 160 characters of its body.
    pub preview: String,
    /// When the phone received it.
    pub received_at: String,
}

/// The fields `PATCH /api/v1/numbers/:key` may change. Written to `inventory_edits` as the
/// request had them, with `e164` normalized, in this order.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct NumberPatch {
    /// The new note, at most 2000 characters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// A normalized E.164 number to set by hand, or `null` to return to the detected one.
    #[serde(skip_serializing_if = "FieldPatch::is_absent")]
    pub e164: FieldPatch<String>,
}
