//! A phone number as other records name it.

use rusqlite::ToSql;
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSqlOutput, ValueRef};
use serde::Serialize;

/// Where a number's E.164 value came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum NumberSource {
    /// The SIM reported it.
    Detected,
    /// A person typed it; detection never overwrites it.
    Manual,
    /// Nobody knows it yet.
    None,
}

impl NumberSource {
    /// The source as the database and the API spell it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Detected => "detected",
            Self::Manual => "manual",
            Self::None => "none",
        }
    }
}

impl FromSql for NumberSource {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        match value.as_str()? {
            "detected" => Ok(Self::Detected),
            "manual" => Ok(Self::Manual),
            "none" => Ok(Self::None),
            other => Err(FromSqlError::Other(
                format!("unknown e164_source {other:?}").into(),
            )),
        }
    }
}

impl ToSql for NumberSource {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::from(self.as_str()))
    }
}

/// A number as a SIM or a phone slot names it: `{"id", "e164", "source", "note"}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct NumberSummary {
    /// The `numbers` row id.
    pub id: i64,
    /// The number in E.164, when known.
    pub e164: Option<String>,
    /// Where `e164` came from.
    pub source: NumberSource,
    /// The owner's note.
    pub note: String,
}
