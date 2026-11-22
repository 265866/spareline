//! Times in reports and alerts, written as the database and the API spell them.

use serde::Serializer;

use crate::Timestamp;

/// Serializes a time as its `Date.toISOString()` form, such as `2026-10-22T08:00:00.000Z`.
#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's serialize_with passes the field by reference"
)]
pub(crate) fn serialize<S: Serializer>(at: &Timestamp, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_str(at)
}
