//! A change of a phone's adb state, as inventory records it.

use spareline_core::Timestamp;

use crate::device_event::NewEvent;
use crate::phone_state::PhoneState;

/// A phone's adb state, seen at a time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhoneStateUpdate {
    /// The adb serial.
    pub serial: String,
    /// The state.
    pub state: PhoneState,
    /// The site a new phone row gets.
    pub site: String,
    /// When it was seen.
    pub at: Timestamp,
    /// The state text exactly as adb printed it, when the state came from an adb device list;
    /// it is added to the status event's detail as `rawState`.
    pub raw_state: Option<String>,
}

/// What recording a state did.
#[derive(Clone, Debug)]
pub struct RecordedState {
    /// The phone's row id, new or existing.
    pub phone_id: i64,
    /// The events to record with [`DeviceEvents::insert`](crate::DeviceEvents::insert).
    pub events: Vec<NewEvent>,
}
