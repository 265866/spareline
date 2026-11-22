//! Hashing, checking and generating passwords.

use std::sync::OnceLock;

use argon2::{Algorithm, Argon2, Params, Version};

use crate::account_error::AccountError;
use crate::password_hash::{Parameters, PasswordHash};
use crate::user::{LoginCandidate, User};
use crate::{js_text, token};

/// The shortest password a person may set: 12 characters, counted in UTF-16 code units as
/// JavaScript counts a string's length.
pub const MIN_PASSWORD_LENGTH: usize = 12;

/// The characters of generated passwords: letters and digits, leaving out `I`, `O`,
/// `l`, `o`, `0` and `1`.
const ALPHABET: &[u8; 56] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnpqrstuvwxyz23456789";

/// The length of generated passwords: 20 characters of 56, about 116 bits.
const GENERATED_LENGTH: usize = 20;

/// The parameters `Bun.password.hash(.., {algorithm: "argon2id"})` uses: 64 MiB, 2 passes, one
/// lane.
const BUN_PARAMETERS: Parameters = Parameters {
    memory_kib: 65_536,
    iterations: 2,
    parallelism: 1,
};

/// The salt and tag length Bun writes, in bytes.
const SALT_LENGTH: usize = 32;
const TAG_LENGTH: usize = 32;

/// Hashes and checks passwords with argon2id, as spareline-old did through `Bun.password`.
///
/// Hashing takes tens of milliseconds and 64 MiB, so a server calls these outside the database
/// lock. One `Passwords` keeps the dummy hash that sign-ins with an unknown name are checked
/// against, so a wrong name takes as long as a wrong password.
#[derive(Debug)]
pub struct Passwords {
    parameters: Parameters,
    dummy: OnceLock<PasswordHash>,
}

impl Passwords {
    /// Hashes with Bun's parameters: argon2id, version 19, `m=65536,t=2,p=1`, a 32-byte salt
    /// and a 32-byte tag.
    #[must_use]
    pub fn new() -> Self {
        Self::with_parameters(BUN_PARAMETERS)
    }

    /// Hashes with `parameters`.
    pub(crate) fn with_parameters(parameters: Parameters) -> Self {
        Self {
            parameters,
            dummy: OnceLock::new(),
        }
    }

    /// Hashes `password` with a new random salt.
    ///
    /// # Errors
    ///
    /// Fails when the operating system's random source does.
    pub fn hash(&self, password: &str) -> Result<PasswordHash, AccountError> {
        let mut salt = [0_u8; SALT_LENGTH];
        token::random_bytes(&mut salt)?;
        let mut tag = [0_u8; TAG_LENGTH];
        argon2(
            Version::V0x13,
            self.parameters,
            TAG_LENGTH,
            password,
            &salt,
            &mut tag,
        )?;
        Ok(PasswordHash::from_parts(self.parameters, &salt, &tag))
    }

    /// Hashes a password a person chose, after checking it has at least
    /// [`MIN_PASSWORD_LENGTH`] characters.
    ///
    /// # Errors
    ///
    /// Fails with [`AccountError::PasswordTooShort`] for a short password, and when the random
    /// source fails.
    pub fn hash_new(&self, password: &str) -> Result<PasswordHash, AccountError> {
        if js_text::utf16_len(password) < MIN_PASSWORD_LENGTH {
            return Err(AccountError::PasswordTooShort);
        }
        self.hash(password)
    }

    /// Whether `password` matches `hash`, with the parameters and salt `hash` names.
    ///
    /// # Errors
    ///
    /// Fails with [`AccountError::UnreadableHash`] when `hash` isn't an argon2id PHC string, or
    /// names parameters argon2 refuses.
    pub fn verify(password: &str, hash: &PasswordHash) -> Result<bool, AccountError> {
        let parsed = hash.parse().ok_or(AccountError::UnreadableHash)?;
        let mut computed = vec![0_u8; parsed.hash.len()];
        argon2(
            parsed.version,
            parsed.parameters,
            parsed.hash.len(),
            password,
            &parsed.salt,
            &mut computed,
        )
        .map_err(|_| AccountError::UnreadableHash)?;
        Ok(constant_time_eq(&computed, &parsed.hash))
    }

    /// The candidate's user when `password` matches its hash. With no candidate, `password` is
    /// checked against a dummy hash, made once, so the answer takes as long as for a known
    /// name.
    ///
    /// # Errors
    ///
    /// Fails when the stored hash is unreadable, or the random source fails while making the
    /// dummy hash.
    pub fn verify_login(
        &self,
        candidate: Option<LoginCandidate>,
        password: &str,
    ) -> Result<Option<User>, AccountError> {
        let Some(candidate) = candidate else {
            let dummy = if let Some(dummy) = self.dummy.get() {
                dummy
            } else {
                let made = self.hash(&Self::generate_password()?)?;
                self.dummy.get_or_init(|| made)
            };
            Self::verify(password, dummy)?;
            return Ok(None);
        };
        Ok(Self::verify(password, &candidate.hash)?.then_some(candidate.user))
    }

    /// A new password of 20 characters, each drawn uniformly from 56 letters and digits by
    /// rejection sampling over bytes from the operating system's random source.
    ///
    /// # Errors
    ///
    /// Fails when the random source does.
    pub fn generate_password() -> Result<String, AccountError> {
        let limit = 256 - 256 % ALPHABET.len();
        let mut password = String::with_capacity(GENERATED_LENGTH);
        while password.len() < GENERATED_LENGTH {
            let mut bytes = [0_u8; GENERATED_LENGTH * 2];
            token::random_bytes(&mut bytes)?;
            for byte in bytes {
                let index = usize::from(byte);
                if index < limit && password.len() < GENERATED_LENGTH {
                    password.push(char::from(ALPHABET[index % ALPHABET.len()]));
                }
            }
        }
        Ok(password)
    }
}

impl Default for Passwords {
    fn default() -> Self {
        Self::new()
    }
}

/// Runs argon2id over `password` and `salt` into `out`.
fn argon2(
    version: Version,
    parameters: Parameters,
    tag_length: usize,
    password: &str,
    salt: &[u8],
    out: &mut [u8],
) -> Result<(), AccountError> {
    let params = Params::new(
        parameters.memory_kib,
        parameters.iterations,
        parameters.parallelism,
        Some(tag_length),
    )
    .map_err(|error| AccountError::Hashing {
        message: error.to_string(),
    })?;
    Argon2::new(Algorithm::Argon2id, version, params)
        .hash_password_into(password.as_bytes(), salt, out)
        .map_err(|error| AccountError::Hashing {
            message: error.to_string(),
        })
}

/// Whether `left` and `right` are equal, in time that depends only on their lengths.
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .fold(0_u8, |difference, (a, b)| difference | (a ^ b))
            == 0
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::test_passwords;

    /// A hash `Bun.password.hash("correct horse battery staple", {algorithm: "argon2id"})` wrote
    /// on Bun 1.4.4.
    const BUN_HASH: &str = "$argon2id$v=19$m=65536,t=2,p=1$sMhWGwGCJAgkRO4i9vESEuHcHxcm+dWGloP5aWJn5I4$F7SuuxEYiFLswyyXhF9QT/7E1Nt5uyO/+foLm+kUKEY";

    #[test]
    fn verifies_a_hash_bun_wrote() {
        let hash = PasswordHash::from_stored(BUN_HASH.to_owned());
        assert!(Passwords::verify("correct horse battery staple", &hash).unwrap());
        assert!(!Passwords::verify("correct horse battery stapler", &hash).unwrap());
    }

    #[test]
    fn writes_bun_parameters() {
        let hash = Passwords::new().hash("a long enough password").unwrap();
        let text = hash.as_str();
        assert!(
            text.starts_with("$argon2id$v=19$m=65536,t=2,p=1$"),
            "{text}"
        );
        let parsed = hash.parse().unwrap();
        assert_eq!(parsed.salt.len(), 32);
        assert_eq!(parsed.hash.len(), 32);
        assert!(!text.ends_with('='), "no base64 padding: {text}");
        assert!(Passwords::verify("a long enough password", &hash).unwrap());
    }

    #[test]
    fn short_passwords_are_refused_by_javascript_length() {
        let passwords = test_passwords::cheap();
        assert!(matches!(
            passwords.hash_new("short"),
            Err(AccountError::PasswordTooShort)
        ));
        let six_emoji = "\u{1f600}".repeat(6);
        assert!(
            passwords.hash_new(&six_emoji).is_ok(),
            "six emoji are 12 UTF-16 units"
        );
        assert!(passwords.hash_new(&"\u{e9}".repeat(11)).is_err());
    }

    #[test]
    fn an_unreadable_hash_is_an_error() {
        let error =
            Passwords::verify("x", &PasswordHash::from_stored("plain".to_owned())).unwrap_err();
        assert!(matches!(error, AccountError::UnreadableHash));
    }

    #[test]
    fn an_unknown_name_is_checked_against_a_dummy_hash() {
        let passwords = test_passwords::cheap();
        assert_eq!(passwords.verify_login(None, "anything").unwrap(), None);
        let first = passwords.dummy.get().cloned();
        assert!(first.is_some(), "the dummy hash is made on first use");
        assert_eq!(passwords.verify_login(None, "anything").unwrap(), None);
        assert_eq!(passwords.dummy.get().cloned(), first, "and kept");
    }

    #[test]
    fn generated_passwords_use_the_whole_alphabet_evenly() {
        let mut counts: HashMap<char, u32> = HashMap::new();
        for _ in 0..2_000 {
            let password = Passwords::generate_password().unwrap();
            assert_eq!(password.len(), 20);
            for character in password.chars() {
                *counts.entry(character).or_default() += 1;
            }
        }
        assert_eq!(counts.len(), 56);
        for (character, count) in counts {
            assert!(
                (550..=900).contains(&count),
                "{character} appeared {count} times of about 714"
            );
            assert!(ALPHABET.contains(&u8::try_from(character).unwrap()));
        }
    }
}
