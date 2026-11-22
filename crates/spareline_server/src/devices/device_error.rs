//! A call to a phone that failed.

use spareline_adb::AdbError;
use thiserror::Error;

/// A call to a phone through [`Devices`](crate::Devices) that failed.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[must_use]
pub enum DeviceError {
    /// The phone isn't online, so no call can reach it.
    #[error("phone {serial} is not online ({state})")]
    NotOnline {
        /// The adb serial.
        serial: String,
        /// Its adb state, or `unknown` for a phone this run hasn't seen.
        state: String,
    },
    /// The phone went offline during the call, which was cancelled.
    #[error("phone {serial} disconnected during the call")]
    Disconnected {
        /// The adb serial.
        serial: String,
    },
    /// The adb server or the phone failed the call.
    #[error(transparent)]
    Adb(#[from] AdbError),
}
