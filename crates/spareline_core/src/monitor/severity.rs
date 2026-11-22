//! How urgent a failing rule or an alert is.

use serde::Serialize;

/// How urgent a failing rule or an alert is, serialized as `critical`, `warning` or `info`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Radios, data, the SMS app and MMS: the phone may cost money or miss texts.
    Critical,
    /// Worth fixing soon.
    Warning,
    /// A card item.
    Info,
}
