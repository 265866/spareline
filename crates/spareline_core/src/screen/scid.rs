//! The session id scrcpy-server names its sockets and processes after.

use std::fmt::{self, Display, Formatter};

use crate::screen::scrcpy::{SCRCPY_VERSION, SERVER_JAR_PHONE_PATH, VIDEO_BIT_RATE};

/// A scrcpy session id: a random 31-bit value written as 8 lowercase hex digits.
///
/// The server names its abstract socket `scrcpy_<scid>`, so sessions on different phones never
/// meet, and the relay kills exactly its own server by matching `scid=<scid>` on its command line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Scid {
    value: u32,
}

impl Scid {
    /// The id for 32 random bits the caller supplies; the top bit is dropped, as scrcpy reads the
    /// id as a positive `int`.
    #[must_use]
    pub const fn from_random(bits: u32) -> Self {
        Self {
            value: bits & 0x7f_ff_ff_ff,
        }
    }

    /// The abstract socket name the server listens on, `scrcpy_<scid>`.
    #[must_use]
    pub fn socket_name(self) -> String {
        format!("scrcpy_{self}")
    }

    /// The server's arguments after its version, exactly as spike S1 measured them: native
    /// resolution at up to 30 fps, 4 Mb/s H.264, no audio, control on, the display kept awake.
    #[must_use]
    pub fn server_args(self) -> Vec<String> {
        vec![
            format!("scid={self}"),
            "log_level=info".to_owned(),
            "video=true".to_owned(),
            "audio=false".to_owned(),
            "control=true".to_owned(),
            "tunnel_forward=true".to_owned(),
            "max_size=0".to_owned(),
            "max_fps=30".to_owned(),
            format!("video_bit_rate={VIDEO_BIT_RATE}"),
            "video_codec=h264".to_owned(),
            "stay_awake=true".to_owned(),
            "clipboard_autosync=false".to_owned(),
            "send_device_meta=true".to_owned(),
            "send_dummy_byte=true".to_owned(),
        ]
    }

    /// The `exec:` command that starts the server, kept open for the whole session.
    #[must_use]
    pub fn server_command(self) -> String {
        let args = self.server_args().join(" ");
        format!(
            "CLASSPATH={SERVER_JAR_PHONE_PATH} app_process / com.genymobile.scrcpy.Server \
             {SCRCPY_VERSION} {args} 2>&1"
        )
    }

    /// A `pgrep -f`/`pkill -f` pattern that matches this server's command line but not the
    /// `sh -c` running the pkill itself: `scid=1234abc[d]` matches `scid=1234abcd`, while the
    /// shell's own argv holds the bracketed text.
    #[must_use]
    pub fn process_pattern(self) -> String {
        let text = self.to_string();
        let (head, last) = text.split_at(text.len() - 1);
        format!("scid={head}[{last}]")
    }

    /// The `exec:` command that kills this session's server at teardown.
    #[must_use]
    pub fn kill_command(self) -> String {
        format!("pkill -f '{}' || true", self.process_pattern())
    }
}

impl Display for Scid {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{:08x}", self.value)
    }
}

#[cfg(test)]
mod tests {
    use regex::Regex;

    use super::*;

    #[test]
    fn random_bits_become_eight_lowercase_hex_digits_without_the_top_bit() {
        assert_eq!(Scid::from_random(0).to_string(), "00000000");
        assert_eq!(Scid::from_random(0x0b_ad_c0_de).to_string(), "0badc0de");
        assert_eq!(Scid::from_random(u32::MAX).to_string(), "7fffffff");
        assert_eq!(Scid::from_random(0x80_00_00_2a).to_string(), "0000002a");
    }

    #[test]
    fn server_arguments_match_the_ones_measured_in_spike_s1() {
        assert_eq!(
            Scid::from_random(0x0b_ad_c0_de).server_args(),
            [
                "scid=0badc0de",
                "log_level=info",
                "video=true",
                "audio=false",
                "control=true",
                "tunnel_forward=true",
                "max_size=0",
                "max_fps=30",
                "video_bit_rate=4000000",
                "video_codec=h264",
                "stay_awake=true",
                "clipboard_autosync=false",
                "send_device_meta=true",
                "send_dummy_byte=true",
            ]
        );
    }

    #[test]
    fn the_launch_command_is_the_exact_string_the_old_relay_ran() {
        assert_eq!(
            Scid::from_random(0x0b_ad_c0_de).server_command(),
            "CLASSPATH=/data/local/tmp/scrcpy-server.jar app_process / \
             com.genymobile.scrcpy.Server 3.3.3 scid=0badc0de log_level=info video=true \
             audio=false control=true tunnel_forward=true max_size=0 max_fps=30 \
             video_bit_rate=4000000 video_codec=h264 stay_awake=true clipboard_autosync=false \
             send_device_meta=true send_dummy_byte=true 2>&1"
        );
        assert_eq!(
            Scid::from_random(0x0b_ad_c0_de).socket_name(),
            "scrcpy_0badc0de"
        );
    }

    #[test]
    fn the_pkill_pattern_matches_the_server_but_not_its_own_shell() {
        let scid = Scid::from_random(0x0b_ad_c0_de);
        assert_eq!(scid.process_pattern(), "scid=0badc0d[e]");
        assert_eq!(scid.kill_command(), "pkill -f 'scid=0badc0d[e]' || true");
        let pattern = Regex::new(&scid.process_pattern()).unwrap();
        assert!(pattern.is_match(
            "app_process / com.genymobile.scrcpy.Server 3.3.3 scid=0badc0de log_level=info"
        ));
        assert!(!pattern.is_match(&format!("sh -c {}", scid.kill_command())));
        assert!(!pattern.is_match("scid=0badc0df"));
    }
}
