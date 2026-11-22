//! The scrcpy control messages a browser viewer may send, and the two the relay sends itself.

/// The limit scrcpy's own client sets for the text of one `INJECT_TEXT` message, in bytes.
pub const MAX_TEXT_BYTES: usize = 300;

/// The largest WebSocket message a viewer may send. The longest allowed control message is 305
/// bytes.
pub const MAX_VIEWER_MESSAGE_BYTES: usize = 4096;

const INJECT_KEYCODE: u8 = 0;
const INJECT_TEXT: u8 = 1;
const INJECT_TOUCH: u8 = 2;
const INJECT_SCROLL: u8 = 3;
const BACK_OR_SCREEN_ON: u8 = 4;
const SET_DISPLAY_POWER: u8 = 10;
const RESET_VIDEO_TYPE: u8 = 17;

/// `RESET_VIDEO`: asks the server for a new config packet and keyframe.
pub const RESET_VIDEO: [u8; 1] = [RESET_VIDEO_TYPE];

/// `SET_DISPLAY_POWER` off, which the relay sends to keep the phone's own panel dark while it is
/// watched. Viewers may not send it.
pub const DISPLAY_POWER_OFF: [u8; 2] = [SET_DISPLAY_POWER, 0];

/// What an allowed control message from a viewer does, for the session's counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlKind {
    /// A key, touch, scroll or back-or-screen-on event, counted in `inputs`.
    Input,
    /// Typed text, counted in `texts`. The text itself is never stored.
    Text,
}

impl ControlKind {
    /// Classifies one WebSocket message from a viewer, or `None` when the relay must drop it and
    /// count it in `rejected`.
    ///
    /// Only key (exactly 14 bytes), text (5 bytes plus at most [`MAX_TEXT_BYTES`], with the
    /// big-endian length at offset 1 matching), touch (32), scroll (21) and back-or-screen-on (2)
    /// pass. Every other type, display power, clipboard, app start, UHID, rotation and panels
    /// included, is refused, and so is a wrong length, which would desynchronize the server's
    /// parser.
    #[must_use]
    pub fn classify(message: &[u8]) -> Option<Self> {
        let length = message.len();
        let input = |expected: usize| (length == expected).then_some(Self::Input);
        match *message.first()? {
            INJECT_KEYCODE => input(14),
            INJECT_TEXT => {
                if !(5..=5 + MAX_TEXT_BYTES).contains(&length) {
                    return None;
                }
                let declared = u32::from_be_bytes([message[1], message[2], message[3], message[4]]);
                (usize::try_from(declared).ok()? == length - 5).then_some(Self::Text)
            }
            INJECT_TOUCH => input(32),
            INJECT_SCROLL => input(21),
            BACK_OR_SCREEN_ON => input(2),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|index| u8::from_str_radix(&text[index..index + 2], 16).unwrap())
            .collect()
    }

    fn text(content: &str) -> Vec<u8> {
        let mut message = vec![INJECT_TEXT];
        message.extend(u32::try_from(content.len()).unwrap().to_be_bytes());
        message.extend(content.as_bytes());
        message
    }

    fn touch() -> Vec<u8> {
        hex("0200fffffffffffffffe00000064000000c800d801e0ffff0000000000000000")
    }

    fn key() -> Vec<u8> {
        hex("0000000000030000000000000000")
    }

    #[test]
    fn allows_key_text_touch_scroll_and_back_or_screen_on() {
        assert_eq!(ControlKind::classify(&key()), Some(ControlKind::Input));
        assert_eq!(
            ControlKind::classify(&hex("010000000668c3a96c6c6f")),
            Some(ControlKind::Text)
        );
        assert_eq!(
            ControlKind::classify(&hex("0100000000")),
            Some(ControlKind::Text)
        );
        assert_eq!(ControlKind::classify(&touch()), Some(ControlKind::Input));
        assert_eq!(
            ControlKind::classify(&hex("0300000064000000c800d801e00000f80000000000")),
            Some(ControlKind::Input)
        );
        assert_eq!(
            ControlKind::classify(&hex("0400")),
            Some(ControlKind::Input)
        );
    }

    #[test]
    fn refuses_every_other_type_display_power_included() {
        for message in [
            hex("0a00"),
            vec![SET_DISPLAY_POWER, 1],
            DISPLAY_POWER_OFF.to_vec(),
            hex("05"),
            hex("06"),
            hex("07"),
            hex("0b"),
            hex("1014636f6d2e616e64726f69642e73657474696e6773"),
            hex("0e0001"),
            hex("11"),
            RESET_VIDEO.to_vec(),
            vec![9, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 1, 0x78],
            vec![8, 0],
            Vec::new(),
        ] {
            assert_eq!(ControlKind::classify(&message), None, "{message:02x?}");
        }
    }

    #[test]
    fn refuses_allowed_types_with_the_wrong_length() {
        let touch = touch();
        let key = key();
        let hello = text("hello");
        for message in [
            touch[..31].to_vec(),
            [touch.as_slice(), &[0]].concat(),
            key[..13].to_vec(),
            [key.as_slice(), key.as_slice()].concat(),
            vec![10],
            hello[..hello.len() - 1].to_vec(),
            [hello.as_slice(), &[0x41]].concat(),
            vec![1, 0, 0, 0],
        ] {
            assert_eq!(ControlKind::classify(&message), None, "{message:02x?}");
        }
    }

    #[test]
    fn refuses_text_over_300_bytes() {
        assert_eq!(
            ControlKind::classify(&text(&"a".repeat(300))),
            Some(ControlKind::Text)
        );
        assert_eq!(text(&"a".repeat(300)).len(), 305);
        assert_eq!(ControlKind::classify(&text(&"a".repeat(301))), None);
    }

    #[test]
    fn the_relay_sends_display_off_and_reset_video_as_scrcpy_spells_them() {
        assert_eq!(DISPLAY_POWER_OFF, [10, 0]);
        assert_eq!(RESET_VIDEO, [17]);
    }
}
