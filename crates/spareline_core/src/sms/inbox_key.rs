//! The provider id and date of every inbox row, for reconciling.

use crate::Timestamp;
use crate::sms::{ContentRow, Provider, provider_message};

/// One inbox row's provider `_id` and receive time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct InboxKey {
    /// The provider's `_id`.
    pub provider_id: i64,
    /// The row's `date`: milliseconds for SMS, seconds for MMS.
    pub received_at: Timestamp,
}

/// The keys of rows of [`INBOX_KEY_COLUMNS`](crate::INBOX_KEY_COLUMNS). A row without an integer
/// `_id`, or without a date after the epoch, can't be fetched or stored, and is skipped.
#[must_use]
pub fn inbox_keys(rows: &[ContentRow], provider: Provider) -> Vec<InboxKey> {
    let scale = match provider {
        Provider::Sms => 1,
        Provider::Mms => 1000,
    };
    rows.iter()
        .filter_map(|row| {
            let provider_id = row.integer("_id")?;
            let received_at = provider_message::provider_time(row.integer("date"), scale)
                .ok()
                .flatten()?;
            Some(InboxKey {
                provider_id,
                received_at,
            })
        })
        .collect()
}
