//! Whether the adb server answers.

use spareline_core::Timestamp;

/// The adb server's reachability, as `/api/v1/status` shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdbStatus {
    /// Whether the last attempt reached it.
    pub reachable: bool,
    /// Its protocol version, such as 41, while reachable.
    pub version: Option<u32>,
    /// When `reachable` last changed, or when this process first learned it.
    pub since: Timestamp,
    /// Why it isn't reachable: `not connected yet` before the first attempt, then the last
    /// error.
    pub error: Option<String>,
}
