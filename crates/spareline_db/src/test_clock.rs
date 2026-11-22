//! Fixed times for tests.

use spareline_core::Timestamp;

/// 2026-10-26T00:00:00.000Z, the time spareline-old's auth tests start at.
pub(crate) const START: Timestamp = Timestamp::from_millis(1_792_972_800_000);

/// Always [`START`], as a clock for [`Database::open`](crate::Database::open).
pub(crate) fn fixed() -> Timestamp {
    START
}

/// The time `text`, in the 24-character form.
pub(crate) fn at(text: &str) -> Timestamp {
    Timestamp::parse_iso(text).expect("a test time is in the 24-character form")
}
