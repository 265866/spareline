//! The check that a program the service pushes to phones is the one it pins.

use sha2::Sha256;

/// A pinned program, such as the scrcpy-server jar or the lock screen dex, whose bytes have
/// another SHA-256 than the one the service pins.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[must_use]
#[error("{what} at {path} has sha256 {digest}, expected {expected}")]
pub struct DigestMismatch {
    what: &'static str,
    path: String,
    digest: String,
    expected: &'static str,
}

impl DigestMismatch {
    /// Checks that `bytes`, read from `path`, hash to `expected`. `what` names the program in the
    /// error, such as `scrcpy-server jar`.
    pub(crate) fn check(
        what: &'static str,
        path: &str,
        bytes: &[u8],
        expected: &'static str,
    ) -> Result<(), Self> {
        let digest = sha256_hex(bytes);
        if digest == expected {
            return Ok(());
        }
        Err(Self {
            what,
            path: path.to_owned(),
            digest,
            expected,
        })
    }
}

/// The SHA-256 of `bytes` as 64 lowercase hex digits.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    use sha2::Digest as _;

    let mut hex = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        write!(hex, "{byte:02x}").expect("writing to a String never fails");
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_the_standard_test_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
