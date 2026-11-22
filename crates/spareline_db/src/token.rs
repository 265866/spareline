//! Random tokens and the SHA-256 digests the database stores instead of them.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::Sha256;

use crate::account_error::AccountError;

/// 32 random bytes from the operating system, as base64url without padding: 43 characters.
///
/// # Errors
///
/// Fails when the operating system's random source does.
pub(crate) fn random_token() -> Result<String, AccountError> {
    use base64::Engine as _;
    let mut bytes = [0_u8; 32];
    random_bytes(&mut bytes)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// Fills `bytes` from the operating system's random source.
///
/// # Errors
///
/// Fails when the random source does.
pub(crate) fn random_bytes(bytes: &mut [u8]) -> Result<(), AccountError> {
    getrandom::fill(bytes).map_err(|error| AccountError::Random {
        message: error.to_string(),
    })
}

/// The lowercase hex SHA-256 of `bytes`. Sessions and API keys store it of their token
/// string's UTF-8 bytes, attachments name their files with it.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(64);
    for byte in digest {
        hex.push(char::from(HEX[usize::from(byte >> 4)]));
        hex.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    hex
}

const HEX: &[u8; 16] = b"0123456789abcdef";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_the_text_not_the_bytes_it_encodes() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn tokens_are_43_base64url_characters() {
        let token = random_token().unwrap();
        assert_eq!(token.len(), 43);
        assert!(
            token
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'),
            "{token}"
        );
        assert_ne!(token, random_token().unwrap());
    }
}
