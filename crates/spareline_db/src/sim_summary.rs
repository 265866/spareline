//! A SIM as other records name it.

use serde::Serialize;

/// A SIM as a placement or a number names it: `{"id", "iccid", "carrier"}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SimSummary {
    /// The `sims` row id.
    pub id: i64,
    /// The ICCID.
    pub iccid: String,
    /// The carrier name the phone reported.
    pub carrier: Option<String>,
}
