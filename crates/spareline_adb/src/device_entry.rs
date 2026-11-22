//! One transport in the adb server's device list.

/// A transport's connection state as the adb server reports it, normalized.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DeviceState {
    /// Online and authorized.
    Device,
    /// Waiting for someone to allow USB debugging on the phone.
    Unauthorized,
    /// Present but not answering.
    Offline,
    /// The host's udev rules or group membership deny USB access.
    NoPermissions,
    /// Connecting.
    Connecting,
    /// Authorizing.
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
    /// A state this client doesn't name; [`DeviceEntry::raw_state`] holds the text.
    Unknown,
}

impl DeviceState {
    /// The state's name as the API and the database spell it, such as `no_permissions`.
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
        }
    }

    /// Normalizes the state text adb prints. `no permissions` arrives with a long help text and
    /// is reduced to its name; a state this client doesn't name is [`DeviceState::Unknown`].
    #[must_use]
    pub fn from_adb(raw: &str) -> Self {
        if raw.starts_with("no permissions") {
            return Self::NoPermissions;
        }
        match raw {
            "device" => Self::Device,
            "unauthorized" => Self::Unauthorized,
            "offline" => Self::Offline,
            "no_permissions" => Self::NoPermissions,
            "connecting" => Self::Connecting,
            "authorizing" => Self::Authorizing,
            "bootloader" => Self::Bootloader,
            "recovery" => Self::Recovery,
            "rescue" => Self::Rescue,
            "sideload" => Self::Sideload,
            "host" => Self::Host,
            "detached" => Self::Detached,
            _ => Self::Unknown,
        }
    }
}

/// One transport from `host:devices-l` or `host:track-devices-l`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceEntry {
    /// The serial adb addresses the phone by.
    pub serial: String,
    /// The normalized state.
    pub state: DeviceState,
    /// The state exactly as adb printed it, kept for `no permissions` and unnamed states.
    pub raw_state: String,
    /// The USB path, such as `1-1`.
    pub usb: Option<String>,
    /// The product name.
    pub product: Option<String>,
    /// The model name.
    pub model: Option<String>,
    /// The device name.
    pub device: Option<String>,
    /// The adb server's id for this connection of the phone, which changes on reconnect.
    pub transport_id: Option<u64>,
}

impl DeviceEntry {
    /// Parses one device-list payload. adb prints each transport as the serial, then the state,
    /// then optional ` key:value` fields, ending with ` transport_id:N`, so the fields are taken
    /// from the end of the line and whatever precedes them is the state.
    #[must_use]
    pub fn parse_list(payload: &str) -> Vec<Self> {
        payload.lines().filter_map(Self::parse_line).collect()
    }

    fn parse_line(line: &str) -> Option<Self> {
        let trimmed = line.trim_end();
        let (serial, rest) = trimmed.split_once(char::is_whitespace)?;
        if serial.is_empty() {
            return None;
        }
        let mut words: Vec<&str> = rest.split_whitespace().collect();
        if words.is_empty() {
            return None;
        }
        let mut entry = Self {
            serial: serial.to_owned(),
            state: DeviceState::Unknown,
            raw_state: String::new(),
            usb: None,
            product: None,
            model: None,
            device: None,
            transport_id: None,
        };
        let mut transport_id = None;
        while words.len() > 1 {
            let Some((key, value)) = words.last().and_then(|word| word.split_once(':')) else {
                break;
            };
            let slot = match key {
                "usb" => &mut entry.usb,
                "product" => &mut entry.product,
                "model" => &mut entry.model,
                "device" => &mut entry.device,
                "transport_id" => &mut transport_id,
                _ => break,
            };
            *slot = Some(value.to_owned());
            words.pop();
        }
        entry.raw_state = words.join(" ");
        entry.state = DeviceState::from_adb(&entry.raw_state);
        entry.transport_id = transport_id.and_then(|id| id.parse().ok());
        Some(entry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_fields_from_the_end_of_the_line() {
        let entries = DeviceEntry::parse_list(
            "SPARELINE00001         device usb:1-1 product:S6702X1 model:S6702X device:S6702XA \
             transport_id:1\n",
        );
        assert_eq!(
            entries,
            vec![DeviceEntry {
                serial: "SPARELINE00001".to_owned(),
                state: DeviceState::Device,
                raw_state: "device".to_owned(),
                usb: Some("1-1".to_owned()),
                product: Some("S6702X1".to_owned()),
                model: Some("S6702X".to_owned()),
                device: Some("S6702XA".to_owned()),
                transport_id: Some(1),
            }]
        );
    }

    #[test]
    fn keeps_a_multi_word_state() {
        let entries = DeviceEntry::parse_list(
            "SPARELINE00002 no permissions (missing udev rules? user is in the plugdev group); \
             see [http://developer.android.com/tools/device.html] usb:1-2 transport_id:7\n\n",
        );
        assert_eq!(entries.len(), 1, "one transport: {entries:?}");
        let entry = &entries[0];
        assert_eq!(entry.state, DeviceState::NoPermissions);
        assert!(
            entry
                .raw_state
                .starts_with("no permissions (missing udev rules?")
        );
        assert_eq!(entry.usb.as_deref(), Some("1-2"));
        assert_eq!(entry.transport_id, Some(7));
    }

    #[test]
    fn an_unnamed_state_is_unknown_and_kept_raw() {
        let entries = DeviceEntry::parse_list("X1 weird-state transport_id:3");
        assert_eq!(entries[0].state, DeviceState::Unknown);
        assert_eq!(entries[0].raw_state, "weird-state");
    }

    #[test]
    fn skips_blank_lines_and_lines_without_a_state() {
        assert_eq!(DeviceEntry::parse_list("\n   \nLONELY\n"), Vec::new());
    }
}
