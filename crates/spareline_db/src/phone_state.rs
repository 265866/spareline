//! A phone's adb state and the status the owner sees.

use serde::Serialize;

/// A phone's connection state as `phones.last_state` stores it: the adb server's normalized
/// state, or `gone` when the adb server doesn't list the phone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PhoneState {
    /// Online and authorized.
    Device,
    /// Waiting for someone to allow USB debugging on the phone.
    Unauthorized,
    /// Present but not answering.
    Offline,
    /// The host's udev rules or group membership deny USB access.
    NoPermissions,
    /// Connecting; transient, never stored.
    Connecting,
    /// Authorizing; transient, never stored.
    Authorizing,
    /// In the bootloader.
    Bootloader,
    /// In recovery.
    Recovery,
    /// In rescue mode.
    Rescue,
    /// In sideload mode.
    Sideload,
    /// The host itself.
    Host,
    /// Detached.
    Detached,
    /// A state adb printed that has no name here.
    Unknown,
    /// Not in the adb server's list.
    Gone,
}

impl PhoneState {
    /// The state as the database and the API spell it, such as `no_permissions`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Device => "device",
            Self::Unauthorized => "unauthorized",
            Self::Offline => "offline",
            Self::NoPermissions => "no_permissions",
            Self::Connecting => "connecting",
            Self::Authorizing => "authorizing",
            Self::Bootloader => "bootloader",
            Self::Recovery => "recovery",
            Self::Rescue => "rescue",
            Self::Sideload => "sideload",
            Self::Host => "host",
            Self::Detached => "detached",
            Self::Unknown => "unknown",
            Self::Gone => "gone",
        }
    }

    /// The status this state shows as.
    #[must_use]
    pub fn status(self) -> PhoneStatus {
        PhoneStatus::of_state(self.as_str())
    }
}

/// How a phone looks to the owner, derived from its adb state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PhoneStatus {
    /// The adb state `device`.
    Online,
    /// The adb state `connecting` or `authorizing`.
    Connecting,
    /// The adb state `unauthorized`.
    NeedsAuth,
    /// The adb state `no_permissions`.
    NoPermissions,
    /// The adb state `offline`, `gone`, `detached` or `unknown`.
    Offline,
    /// Any other state, such as `bootloader` or `recovery`.
    Other,
}

impl PhoneStatus {
    /// The status of a stored `last_state`, as spareline-old's `phoneStatus` maps it. Text it
    /// doesn't name is [`PhoneStatus::Other`].
    #[must_use]
    pub fn of_state(state: &str) -> Self {
        match state {
            "device" => Self::Online,
            "unauthorized" => Self::NeedsAuth,
            "no_permissions" => Self::NoPermissions,
            "connecting" | "authorizing" => Self::Connecting,
            "offline" | "gone" | "detached" | "unknown" => Self::Offline,
            _ => Self::Other,
        }
    }

    /// The status as the API spells it, such as `needs_auth`.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_follow_spareline_old() {
        let cases = [
            (PhoneState::Device, PhoneStatus::Online),
            (PhoneState::Unauthorized, PhoneStatus::NeedsAuth),
            (PhoneState::NoPermissions, PhoneStatus::NoPermissions),
            (PhoneState::Connecting, PhoneStatus::Connecting),
            (PhoneState::Authorizing, PhoneStatus::Connecting),
            (PhoneState::Offline, PhoneStatus::Offline),
            (PhoneState::Gone, PhoneStatus::Offline),
            (PhoneState::Detached, PhoneStatus::Offline),
            (PhoneState::Unknown, PhoneStatus::Offline),
            (PhoneState::Bootloader, PhoneStatus::Other),
            (PhoneState::Recovery, PhoneStatus::Other),
            (PhoneState::Rescue, PhoneStatus::Other),
            (PhoneState::Sideload, PhoneStatus::Other),
            (PhoneState::Host, PhoneStatus::Other),
        ];
        for (state, status) in cases {
            assert_eq!(state.status(), status, "{}", state.as_str());
        }
        assert_eq!(PhoneStatus::of_state("something new"), PhoneStatus::Other);
        assert_eq!(
            serde_json::to_string(&PhoneStatus::NeedsAuth).unwrap(),
            r#""needs_auth""#
        );
    }
}
