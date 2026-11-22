//! How a phone looks to the owner, from its adb state.

use serde::Serialize;

/// How a phone looks to the owner, serialized as its snake-case name, such as `needs_auth`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PhoneStatus {
    /// The adb state `device`.
    Online,
    /// Connecting or authorizing.
    Connecting,
    /// Waiting for someone to tap Allow USB debugging on the phone.
    NeedsAuth,
    /// The host's udev rules or plugdev group deny USB access.
    NoPermissions,
    /// Not connected.
    Offline,
    /// Another mode, such as the bootloader or recovery.
    Other,
}

impl PhoneStatus {
    /// The status's name as the API spells it, such as `needs_auth`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Online => "online",
            Self::Connecting => "connecting",
            Self::NeedsAuth => "needs_auth",
            Self::NoPermissions => "no_permissions",
            Self::Offline => "offline",
            Self::Other => "other",
        }
    }
}

/// The status of a phone in an adb state, as the API and the database spell it: a normalized
/// adb state, such as `device` or `no_permissions`, or `gone` for a phone adb doesn't list.
/// `bootloader`, `recovery`, `rescue`, `sideload`, `host` and any state this doesn't name are
/// [`PhoneStatus::Other`].
#[must_use]
pub fn phone_status(state: &str) -> PhoneStatus {
    match state {
        "device" => PhoneStatus::Online,
        "unauthorized" => PhoneStatus::NeedsAuth,
        "no_permissions" => PhoneStatus::NoPermissions,
        "connecting" | "authorizing" => PhoneStatus::Connecting,
        "offline" | "gone" | "detached" | "unknown" => PhoneStatus::Offline,
        _ => PhoneStatus::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_adb_state() {
        let cases = [
            ("device", "online"),
            ("unauthorized", "needs_auth"),
            ("no_permissions", "no_permissions"),
            ("connecting", "connecting"),
            ("authorizing", "connecting"),
            ("offline", "offline"),
            ("gone", "offline"),
            ("detached", "offline"),
            ("unknown", "offline"),
            ("bootloader", "other"),
            ("recovery", "other"),
            ("rescue", "other"),
            ("sideload", "other"),
            ("host", "other"),
            ("no permissions; see udev", "other"),
            ("", "other"),
        ];
        for (state, status) in cases {
            assert_eq!(
                phone_status(state).as_str(),
                status,
                "the status of {state:?}"
            );
        }
    }

    #[test]
    fn serializes_as_its_name() {
        assert_eq!(
            serde_json::to_string(&PhoneStatus::NeedsAuth).expect("serializes"),
            r#""needs_auth""#
        );
        assert_eq!(
            serde_json::to_string(&PhoneStatus::NoPermissions).expect("serializes"),
            r#""no_permissions""#
        );
    }
}
