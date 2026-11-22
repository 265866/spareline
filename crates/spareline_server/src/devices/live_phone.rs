//! What the phone manager knows of a phone this run.

use spareline_core::{PhoneHealth, PhoneIdentity, PhoneStatus, Timestamp};
use spareline_db::PhoneState;

/// A phone the manager has seen this run, with its last identity and health probes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LivePhone {
    /// The adb serial.
    pub serial: String,
    /// The adb state, or [`PhoneState::Gone`] once adb no longer lists the phone.
    pub adb_state: PhoneState,
    /// How the phone looks to the owner, from `adb_state`.
    pub status: PhoneStatus,
    /// When `status` last changed.
    pub since: Timestamp,
    /// The USB path adb last reported, such as `1-1`.
    pub usb: Option<String>,
    /// The last identity probe's result.
    pub identity: Option<PhoneIdentity>,
    /// When `identity` was read.
    pub identity_at: Option<Timestamp>,
    /// The last health probe's result.
    pub health: Option<PhoneHealth>,
    /// When `health` was read.
    pub health_at: Option<Timestamp>,
    /// Why the last probe failed, until one succeeds or the phone reconnects.
    pub probe_error: Option<String>,
}
