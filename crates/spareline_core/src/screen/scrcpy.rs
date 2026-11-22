//! The vendored scrcpy-server and the shell commands the relay runs around it.

use std::sync::LazyLock;

use regex::Regex;

use crate::DigestMismatch;

/// The scrcpy-server version the relay speaks, which the server checks against its argument.
pub const SCRCPY_VERSION: &str = "3.3.3";

/// Where the jar goes on the phone. The server's settings-restoring `CleanUp` child runs with
/// this exact `CLASSPATH`, so the jar must live here and nowhere else. The server unlinks it at
/// startup, so every session pushes it again, with mode 0644.
pub const SERVER_JAR_PHONE_PATH: &str = "/data/local/tmp/scrcpy-server.jar";

/// The SHA-256 of scrcpy-server v3.3.3 as the release publishes it; the service refuses any
/// other jar.
pub const SERVER_JAR_SHA256: &str =
    "7e70323ba7f259649dd4acce97ac4fefbae8102b2c6d91e2e7be613fd5354be0";

/// The video bit rate, in bits per second: native resolution at up to 30 fps, measured in
/// spike S1.
pub const VIDEO_BIT_RATE: u32 = 4_000_000;

/// A `pgrep -f` pattern for any scrcpy process: a server, or `CleanUp` restoring settings after
/// one. The brackets keep it from matching the shell that runs the `pgrep`.
pub const ANY_SCRCPY_PATTERN: &str = "com.genymobile.scrcp[y]";

/// A `pkill -f` pattern for any scrcpy server, whatever its session id; `CleanUp` does not
/// match.
pub const ANY_SERVER_PATTERN: &str = "com.genymobile.scrcpy.Serve[r]";

/// The first step of a start: kill any server left on the phone, an orphan from a crashed relay
/// or someone's own scrcpy, so its `CleanUp` restores the settings it changed.
pub const KILL_ANY_SERVER_COMMAND: &str = "pkill -f 'com.genymobile.scrcpy.Serve[r]' || true";

/// Polled every 50 ms after [`KILL_ANY_SERVER_COMMAND`] until it prints nothing, for up to 3 s,
/// so a new server never records another server's changed settings as the ones to restore.
pub const LIST_ANY_SCRCPY_COMMAND: &str = "pgrep -f 'com.genymobile.scrcp[y]' || true";

/// Wakes the phone and dismisses a swipe keyguard, so the screen shows something that changes.
/// A dozing phone shows scrcpy a dark display and no frames would flow. It touches no radio.
pub const WAKE_COMMAND: &str = "input keyevent KEYCODE_WAKEUP; wm dismiss-keyguard";

/// One cheap read of whether the phone is awake, about 56 ms through adb on the B30.
pub const WAKEFULNESS_COMMAND: &str = "dumpsys power | grep -m1 mWakefulness=";

static WAKEFULNESS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"mWakefulness=([A-Za-z0-9_]+)").expect("a valid pattern"));

/// The wakefulness [`WAKEFULNESS_COMMAND`] printed: `Awake`, `Dozing`, `Asleep` or `Dreaming`,
/// or `None` when the output holds none.
#[must_use]
pub fn parse_wakefulness(output: &str) -> Option<&str> {
    WAKEFULNESS
        .captures(output)
        .and_then(|captures| captures.get(1))
        .map(|state| state.as_str())
}

/// Checks that `jar`, read from `path`, is the pinned scrcpy-server.
///
/// # Errors
///
/// [`DigestMismatch`] when its SHA-256 isn't [`SERVER_JAR_SHA256`].
pub fn verify_server_jar(path: &str, jar: &[u8]) -> Result<(), DigestMismatch> {
    DigestMismatch::check("scrcpy-server jar", path, jar, SERVER_JAR_SHA256)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wakefulness_read_parses_what_the_b30_printed_awake_and_dozing() {
        assert_eq!(
            parse_wakefulness(include_str!(
                "../../tests/fixtures/nuu-s6702x/wakefulness-awake.txt"
            )),
            Some("Awake")
        );
        assert_eq!(
            parse_wakefulness(include_str!(
                "../../tests/fixtures/nuu-s6702x/wakefulness-dozing.txt"
            )),
            Some("Dozing")
        );
        assert_eq!(parse_wakefulness(""), None);
    }

    #[test]
    fn the_settle_commands_use_the_bracketed_patterns() {
        assert_eq!(
            KILL_ANY_SERVER_COMMAND,
            format!("pkill -f '{ANY_SERVER_PATTERN}' || true")
        );
        assert_eq!(
            LIST_ANY_SCRCPY_COMMAND,
            format!("pgrep -f '{ANY_SCRCPY_PATTERN}' || true")
        );
    }

    #[test]
    fn only_the_pinned_jar_passes() {
        let error = verify_server_jar("/x/jar", b"not the jar").unwrap_err();
        assert_eq!(
            error.to_string(),
            "scrcpy-server jar at /x/jar has sha256 \
             eb01b0ab55c1f6466ee396dba56675cd39642a258ba0f2924b85cb775e5fd4da, expected \
             7e70323ba7f259649dd4acce97ac4fefbae8102b2c6d91e2e7be613fd5354be0"
        );
    }
}
