//! The lock screen notice: `lockscreen.dex` draws the profile's text and sets it as the lock
//! screen wallpaper under a name `dumpsys wallpaper` prints, so the drift check can tell the
//! notice is still there.

use std::iter;

use serde::Deserialize;

use crate::{DigestMismatch, digest_mismatch};

/// The SHA-256 of `lockscreen.dex`, built from spareline-old's `android/spareline/LockScreen.java`
/// by its `scripts/build-android.ts`, which reproduces this digest. The service refuses any other
/// program.
pub const LOCK_SCREEN_DEX_SHA256: &str =
    "e6f8ce2c53f58361999eaa2259c4c09dab4126d7df1e939c36be97493c6ac65b";

/// Where the program goes on the phone, pushed with mode 0644. It deletes itself when it starts,
/// so every run pushes it again.
pub const LOCK_SCREEN_DEX_PHONE_PATH: &str = "/data/local/tmp/spareline-lockscreen.dex";

/// The notice's text: a title and up to three lines, each trimmed, non-empty and one line.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockScreenNotice {
    /// The title, at most 40 characters.
    #[serde(default = "default_title")]
    pub title: String,
    /// The lines under it, at most three of at most 100 characters each.
    #[serde(default = "default_lines")]
    pub lines: Vec<String>,
}

fn default_title() -> String {
    "Managed by spareline".to_owned()
}

fn default_lines() -> Vec<String> {
    vec![
        "This phone is part of a running setup.".to_owned(),
        "Please leave it plugged in and turned on.".to_owned(),
    ]
}

impl Default for LockScreenNotice {
    fn default() -> Self {
        Self {
            title: default_title(),
            lines: default_lines(),
        }
    }
}

impl LockScreenNotice {
    /// The wallpaper name the program sets: `spareline-` and the first 16 hex digits of the
    /// SHA-256 of the program's digest, the title and the lines, joined with newlines. It changes
    /// with the text and with the program, so after either changes the check fails until Set up
    /// phone draws the notice again.
    #[must_use]
    pub fn name(&self) -> String {
        let text = self.joined_with(LOCK_SCREEN_DEX_SHA256);
        let digest = digest_mismatch::sha256_hex(text.as_bytes());
        format!("spareline-{}", &digest[..16])
    }

    /// The command that runs the program after [`LOCK_SCREEN_DEX_PHONE_PATH`] is pushed. The text
    /// travels as base64 of its UTF-8, which keeps it out of the shell's quoting.
    #[must_use]
    pub fn command(&self) -> String {
        let mut text = self.title.clone();
        for line in &self.lines {
            text.push('\n');
            text.push_str(line);
        }
        format!(
            "CLASSPATH={LOCK_SCREEN_DEX_PHONE_PATH} app_process / spareline.LockScreen {} {}",
            self.name(),
            base64(text.as_bytes())
        )
    }

    fn joined_with(&self, first: &str) -> String {
        let mut text = first.to_owned();
        for part in iter::once(&self.title).chain(&self.lines) {
            text.push('\n');
            text.push_str(part);
        }
        text
    }
}

/// Checks that `dex`, read from `path`, is the pinned lock screen program.
///
/// # Errors
///
/// [`DigestMismatch`] when its SHA-256 isn't [`LOCK_SCREEN_DEX_SHA256`].
pub fn verify_lock_screen_dex(path: &str, dex: &[u8]) -> Result<(), DigestMismatch> {
    DigestMismatch::check("lock screen program", path, dex, LOCK_SCREEN_DEX_SHA256)
}

/// Standard base64 with padding, as Node's `Buffer.toString("base64")` writes it.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut text = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let group = chunk
            .iter()
            .enumerate()
            .fold(0_u32, |group, (index, &byte)| {
                group | u32::from(byte) << (16 - 8 * index)
            });
        for index in 0..4 {
            if index <= chunk.len() {
                let sextet = (group >> (18 - 6 * index)) & 0x3f;
                text.push(char::from(ALPHABET[sextet as usize]));
            } else {
                text.push('=');
            }
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor;

    #[test]
    fn the_default_notice_has_the_name_the_b30_shows() {
        let notice = LockScreenNotice::default();
        assert_eq!(notice.name(), "spareline-7d53969d19f0d42e");
    }

    #[test]
    fn the_notices_name_changes_with_its_text() {
        let notice = LockScreenNotice::default();
        let name = notice.name();
        assert!(name.starts_with("spareline-"));
        assert_eq!(name.len(), "spareline-".len() + 16);
        assert!(
            name["spareline-".len()..]
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        );
        assert_eq!(notice.clone().name(), name);
        let retitled = LockScreenNotice {
            title: "Property of the lab".to_owned(),
            ..notice.clone()
        };
        assert_ne!(retitled.name(), name);
        let shorter = LockScreenNotice {
            lines: notice.lines[1..].to_vec(),
            ..notice
        };
        assert_ne!(shorter.name(), name);
    }

    #[test]
    fn the_command_carries_the_text_as_base64_and_passes_the_radio_guard() {
        let notice = LockScreenNotice {
            title: "Owner's phone; don't `unplug`".to_owned(),
            lines: vec!["$HOME & ünïcode".to_owned()],
        };
        let command = notice.command();
        let words: Vec<&str> = command.split(' ').collect();
        assert_eq!(
            words[..4],
            [
                "CLASSPATH=/data/local/tmp/spareline-lockscreen.dex",
                "app_process",
                "/",
                "spareline.LockScreen"
            ]
        );
        assert_eq!(words.len(), 6);
        assert_eq!(words[4], notice.name());
        assert_eq!(
            words[5],
            "T3duZXIncyBwaG9uZTsgZG9uJ3QgYHVucGx1Z2AKJEhPTUUgJiDDvG7Dr2NvZGU="
        );
        assert_eq!(monitor::radio_hazard(&command), None);
    }

    #[test]
    fn base64_pads_like_node() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"a"), "YQ==");
        assert_eq!(base64(b"ab"), "YWI=");
        assert_eq!(base64(b"abc"), "YWJj");
        assert_eq!(base64(b"abcd"), "YWJjZA==");
        assert_eq!(base64(&[0xff, 0xfe, 0xfd]), "//79");
    }

    #[test]
    fn only_the_pinned_program_passes() {
        assert_eq!(
            verify_lock_screen_dex("/x/other.dex", b"dex\n039\0")
                .unwrap_err()
                .to_string()
                .split(" has sha256 ")
                .next(),
            Some("lock screen program at /x/other.dex")
        );
    }
}
