//! The two programs spareline runs on phones, built into the binary so the image needs no files
//! beside it.

/// The scrcpy-server 3.3.3 program, pushed to a phone while someone watches its screen. Apache-2.0, by
/// Genymobile and Romain Vimont; its license is in `assets/scrcpy-server-v3.3.3/LICENSE`.
pub const SCRCPY_SERVER: &[u8] =
    include_bytes!("../assets/scrcpy-server-v3.3.3/scrcpy-server-v3.3.3");

/// The SHA-256 of [`SCRCPY_SERVER`], as scrcpy's release lists it.
pub const SCRCPY_SERVER_SHA256: &str =
    "7e70323ba7f259649dd4acce97ac4fefbae8102b2c6d91e2e7be613fd5354be0";

/// The lock screen notice program, built from `assets/lockscreen/LockScreen.java` with d8, which
/// sets a phone's lock screen wallpaper and touches nothing else.
pub const LOCK_SCREEN_DEX: &[u8] = include_bytes!("../assets/lockscreen/lockscreen.dex");

/// The SHA-256 of [`LOCK_SCREEN_DEX`]. The notice's name on a phone is derived from it, so the
/// policy can tell a phone shows the current notice.
pub const LOCK_SCREEN_DEX_SHA256: &str =
    "e6f8ce2c53f58361999eaa2259c4c09dab4126d7df1e939c36be97493c6ac65b";

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use sha2::{Digest, Sha256};

    use super::*;

    fn sha256_hex(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .fold(String::new(), |mut hex, byte| {
                write!(hex, "{byte:02x}").unwrap();
                hex
            })
    }

    #[test]
    fn the_built_in_programs_match_their_pinned_hashes() {
        assert_eq!(sha256_hex(SCRCPY_SERVER), SCRCPY_SERVER_SHA256);
        assert_eq!(sha256_hex(LOCK_SCREEN_DEX), LOCK_SCREEN_DEX_SHA256);
    }
}
