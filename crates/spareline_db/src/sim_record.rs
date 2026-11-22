//! A SIM as the database knows it, served unchanged by `GET /api/v1/sims`.

use rusqlite::ToSql;
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSqlOutput, ValueRef};
use serde::Serialize;

use crate::number_summary::NumberSummary;

/// A SIM's status, set by people.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SimStatus {
    /// In use.
    Active,
    /// Kept for later.
    Spare,
    /// No longer used.
    Retired,
}

impl SimStatus {
    /// The status as the database and the API spell it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Spare => "spare",
            Self::Retired => "retired",
        }
    }
}

impl FromSql for SimStatus {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        match value.as_str()? {
            "active" => Ok(Self::Active),
            "spare" => Ok(Self::Spare),
            "retired" => Ok(Self::Retired),
            other => Err(FromSqlError::Other(
                format!("unknown SIM status {other:?}").into(),
            )),
        }
    }
}

impl ToSql for SimStatus {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::from(self.as_str()))
    }
}

/// A SIM with where it is and its number, the API's `Sim`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SimRecord {
    /// The row id.
    pub id: i64,
    /// The ICCID.
    pub iccid: String,
    /// The carrier name the phone reported.
    pub carrier: Option<String>,
    /// Set by people.
    pub status: SimStatus,
    /// When it was first seen.
    pub first_seen_at: String,
    /// When it was last seen.
    pub last_seen_at: String,
    /// Where it is now.
    pub placement: Option<SimPlacement>,
    /// Where it was last, only while it is in no phone.
    pub last_placement: Option<LastPlacement>,
    /// The number its open binding names.
    pub number: Option<NumberSummary>,
}

/// Where a SIM is now.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SimPlacement {
    /// The phone's row id.
    pub phone_id: i64,
    /// The phone's serial.
    pub serial: String,
    /// The slot.
    pub slot: i64,
    /// Android's subscription id.
    pub sub_id: i64,
    /// Since when.
    pub since: String,
}

/// Where a SIM was last.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LastPlacement {
    /// The phone's serial.
    pub serial: String,
    /// The slot.
    pub slot: i64,
    /// When it left.
    pub until: String,
}

/// The fields `PATCH /api/v1/sims/:key` may change.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SimPatch {
    /// The new status.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<SimStatus>,
}
