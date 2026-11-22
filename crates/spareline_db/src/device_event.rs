//! Device events: what happened to phones, SIMs and the adb server, for the event stream and the
//! audit trail.

use serde::Serialize;
use serde_json::value::RawValue;

use crate::database_error::DatabaseError;

/// An event to record, before it has an id. Its `detail` is JSON text, written to
/// `device_events.detail_json` and served on the event stream exactly as it is.
#[derive(Clone, Debug)]
pub struct NewEvent {
    /// The phone it is about, or `None` for events about the adb server.
    pub phone_id: Option<i64>,
    /// What happened, such as `phone.online`; surface.md section 7.3 lists them.
    pub kind: String,
    /// The details, a JSON object.
    pub detail: Box<RawValue>,
}

impl NewEvent {
    /// An event whose detail is `detail` written as JSON, in its fields' order.
    ///
    /// # Errors
    ///
    /// Fails when `detail` can't be written as JSON, which a plain struct of strings and
    /// numbers never causes.
    pub fn try_new(
        phone_id: Option<i64>,
        kind: &str,
        detail: &impl Serialize,
    ) -> Result<Self, DatabaseError> {
        let text = serde_json::to_string(detail).map_err(|source| {
            DatabaseError::json(format!("the detail of a {kind} event"), source)
        })?;
        let detail = RawValue::from_string(text).map_err(|source| {
            DatabaseError::json(format!("the detail of a {kind} event"), source)
        })?;
        Ok(Self {
            phone_id,
            kind: kind.to_owned(),
            detail,
        })
    }

    /// `adb.reachable`, with `{"error": null}`.
    ///
    /// # Errors
    ///
    /// Never fails in practice; see [`NewEvent::try_new`].
    pub fn adb_reachable() -> Result<Self, DatabaseError> {
        Self::try_new(None, "adb.reachable", &AdbDetail { error: None })
    }

    /// `adb.unreachable`, with `{"error": <why>}`.
    ///
    /// # Errors
    ///
    /// Never fails in practice; see [`NewEvent::try_new`].
    pub fn adb_unreachable(error: &str) -> Result<Self, DatabaseError> {
        Self::try_new(None, "adb.unreachable", &AdbDetail { error: Some(error) })
    }
}

/// The detail of `adb.reachable` and `adb.unreachable`.
#[derive(Serialize)]
struct AdbDetail<'detail> {
    error: Option<&'detail str>,
}

/// A recorded device event, as `event: device` on the event stream carries it:
/// `{"id", "phoneId", "serial", "at", "kind", "detail"}`.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceEvent {
    /// The row id, which is the stream's device cursor.
    pub id: i64,
    /// The phone it is about.
    pub phone_id: Option<i64>,
    /// That phone's serial.
    pub serial: Option<String>,
    /// When it happened.
    pub at: String,
    /// What happened.
    pub kind: String,
    /// The details, as stored.
    pub detail: Box<RawValue>,
}

/// The last `provision` event of a phone, the parts spareline-old read back to rebuild its last
/// provision report after a restart. Missing fields take spareline-old's defaults.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LastProvision {
    /// When it ran.
    pub at: String,
    /// Whether it succeeded; `false` when the detail lacks it.
    pub ok: bool,
    /// The rules it changed; empty when the detail lacks them.
    pub changed: Vec<String>,
    /// How long it took; 0 when the detail lacks it.
    pub duration_ms: i64,
    /// Why it failed.
    pub error: Option<String>,
}
