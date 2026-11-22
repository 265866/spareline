//! Password hashes: argon2id, in the PHC string format Bun writes.

use std::fmt::{self, Debug, Formatter};

use argon2::Version;
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD_NO_PAD, STANDARD_NO_PAD_INDIFFERENT};

/// An argon2id hash in the PHC string format, as `users.password_hash` holds it:
/// `$argon2id$v=19$m=65536,t=2,p=1$<salt>$<hash>`, with the salt and hash in standard base64
/// without padding. Its [`Debug`] hides the string.
#[derive(Clone, PartialEq, Eq)]
pub struct PasswordHash(String);

impl PasswordHash {
    /// A hash read from the database, unchecked until it is verified against.
    pub(crate) fn from_stored(text: String) -> Self {
        Self(text)
    }

    /// The PHC string written from its parts.
    pub(crate) fn from_parts(parameters: Parameters, salt: &[u8], hash: &[u8]) -> Self {
        let Parameters {
            memory_kib,
            iterations,
            parallelism,
        } = parameters;
        Self(format!(
            "$argon2id$v=19$m={memory_kib},t={iterations},p={parallelism}${}${}",
            STANDARD_NO_PAD.encode(salt),
            STANDARD_NO_PAD.encode(hash)
        ))
    }

    /// The PHC string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The parts of the PHC string, or `None` when it isn't an argon2id PHC string.
    ///
    /// The parameters may come in any order, the version is `v=19` or `v=16` and defaults to
    /// 16 when absent, as the reference implementation reads it, and the base64 may carry
    /// padding.
    pub(crate) fn parse(&self) -> Option<ParsedHash> {
        let rest = self.0.strip_prefix("$argon2id$")?;
        let fields: Vec<&str> = rest.split('$').collect();
        let (version, parameters, salt, hash) = match fields.as_slice() {
            [version, parameters, salt, hash] => (version_of(version)?, *parameters, *salt, *hash),
            [parameters, salt, hash] if !parameters.starts_with("v=") => {
                (Version::V0x10, *parameters, *salt, *hash)
            }
            _ => return None,
        };
        let parameters = parameters_of(parameters)?;
        let salt = STANDARD_NO_PAD_INDIFFERENT.decode(salt).ok()?;
        let hash = STANDARD_NO_PAD_INDIFFERENT.decode(hash).ok()?;
        if salt.is_empty() || hash.is_empty() {
            return None;
        }
        Some(ParsedHash {
            version,
            parameters,
            salt,
            hash,
        })
    }
}

impl Debug for PasswordHash {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str("PasswordHash(..)")
    }
}

/// The cost parameters of argon2.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Parameters {
    pub(crate) memory_kib: u32,
    pub(crate) iterations: u32,
    pub(crate) parallelism: u32,
}

/// The parts of a PHC string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ParsedHash {
    pub(crate) version: Version,
    pub(crate) parameters: Parameters,
    pub(crate) salt: Vec<u8>,
    pub(crate) hash: Vec<u8>,
}

fn version_of(field: &str) -> Option<Version> {
    match field {
        "v=19" => Some(Version::V0x13),
        "v=16" => Some(Version::V0x10),
        _ => None,
    }
}

/// Reads `m=..,t=..,p=..`, in any order, each exactly once.
fn parameters_of(field: &str) -> Option<Parameters> {
    let mut memory_kib = None;
    let mut iterations = None;
    let mut parallelism = None;
    for pair in field.split(',') {
        let (key, value) = pair.split_once('=')?;
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let number: u32 = value.parse().ok()?;
        let slot = match key {
            "m" => &mut memory_kib,
            "t" => &mut iterations,
            "p" => &mut parallelism,
            _ => return None,
        };
        if slot.replace(number).is_some() {
            return None;
        }
    }
    Some(Parameters {
        memory_kib: memory_kib?,
        iterations: iterations?,
        parallelism: parallelism?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_what_bun_writes() {
        let hash = PasswordHash::from_stored(
            "$argon2id$v=19$m=65536,t=2,p=1$sMhWGwGCJAgkRO4i9vESEuHcHxcm+dWGloP5aWJn5I4$F7SuuxEYiFLswyyXhF9QT/7E1Nt5uyO/+foLm+kUKEY"
                .to_owned(),
        );
        let parsed = hash.parse().unwrap();
        assert_eq!(parsed.version, Version::V0x13);
        assert_eq!(
            parsed.parameters,
            Parameters {
                memory_kib: 65536,
                iterations: 2,
                parallelism: 1
            }
        );
        assert_eq!(parsed.salt.len(), 32);
        assert_eq!(parsed.hash.len(), 32);
    }

    #[test]
    fn reads_parameters_in_any_order_and_writes_them_back() {
        let hash = PasswordHash::from_stored(
            "$argon2id$v=19$p=1,t=3,m=4096$c2FsdHNhbHQ$aGFzaGhhc2g".to_owned(),
        );
        let parsed = hash.parse().unwrap();
        assert_eq!(parsed.parameters.memory_kib, 4096);
        assert_eq!(parsed.parameters.iterations, 3);
        let written = PasswordHash::from_parts(parsed.parameters, &parsed.salt, &parsed.hash);
        assert_eq!(
            written.as_str(),
            "$argon2id$v=19$m=4096,t=3,p=1$c2FsdHNhbHQ$aGFzaGhhc2g"
        );
    }

    #[test]
    fn refuses_other_strings() {
        for text in [
            "$argon2i$v=19$m=4096,t=3,p=1$c2FsdHNhbHQ$aGFzaGhhc2g",
            "$argon2id$v=20$m=4096,t=3,p=1$c2FsdHNhbHQ$aGFzaGhhc2g",
            "$argon2id$v=19$m=4096,t=3$c2FsdHNhbHQ$aGFzaGhhc2g",
            "$argon2id$v=19$m=4096,t=3,p=1,p=1$c2FsdHNhbHQ$aGFzaGhhc2g",
            "$argon2id$v=19$m=4096,t=3,p=1,x=1$c2FsdHNhbHQ$aGFzaGhhc2g",
            "$argon2id$v=19$m=+4096,t=3,p=1$c2FsdHNhbHQ$aGFzaGhhc2g",
            "$argon2id$v=19$m=4096,t=3,p=1$c2FsdHNhbHQ",
            "$argon2id$v=19$m=4096,t=3,p=1$!!$aGFzaGhhc2g",
            "$2b$10$abcdefghijklmnopqrstuuABCDEFGHIJKLMNOPQRSTUVWXYZ01234",
            "",
        ] {
            assert_eq!(
                PasswordHash::from_stored(text.to_owned()).parse(),
                None,
                "{text}"
            );
        }
        let unversioned = PasswordHash::from_stored(
            "$argon2id$m=4096,t=3,p=1$c2FsdHNhbHQ$aGFzaGhhc2g".to_owned(),
        );
        assert_eq!(unversioned.parse().unwrap().version, Version::V0x10);
    }

    #[test]
    fn debug_hides_the_hash() {
        let hash = PasswordHash::from_stored("$argon2id$secret".to_owned());
        assert_eq!(format!("{hash:?}"), "PasswordHash(..)");
    }
}
