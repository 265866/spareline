//! The error every adb call returns.

use thiserror::Error;

/// What kind of failure an [`AdbError`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdbErrorKind {
    /// The adb server isn't reachable.
    Connect,
    /// The call took longer than its deadline.
    Timeout,
    /// The peer closed the connection before the expected bytes arrived.
    Closed,
    /// The bytes don't follow the adb wire format.
    Protocol,
    /// The adb server or the phone answered `FAIL` with a message.
    Fail,
}

/// A failed adb call, with a message that names the request and, for `FAIL`, the server's reason.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error("{message}")]
#[must_use]
pub struct AdbError {
    kind: AdbErrorKind,
    message: String,
}

impl AdbError {
    /// An error of `kind` with `message`, which is shown as it is.
    pub fn new(kind: AdbErrorKind, message: String) -> Self {
        Self { kind, message }
    }

    /// What kind of failure this is.
    #[must_use]
    pub fn kind(&self) -> AdbErrorKind {
        self.kind
    }

    /// The message, as `Display` shows it.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}
