//! The error account calls return.

use thiserror::Error;

use crate::database_error::DatabaseError;

/// A failed account call. The messages of the variants up to [`AccountError::NoApiKey`] are
/// spareline-old's `AuthError` messages, which reach the API and the command line as they are;
/// [`AccountError::is_user_error`] tells them apart from internal failures.
#[derive(Debug, Error)]
#[must_use]
pub enum AccountError {
    /// A new user's name isn't 1-64 ASCII letters, digits, `.`, `_`, `@` and `-`.
    #[error("a user name is 1-64 letters, digits, and . _ @ - characters")]
    InvalidUserName,
    /// A user with the name exists already, compared without ASCII case.
    #[error("user {name} already exists")]
    UserExists {
        /// The trimmed name.
        name: String,
    },
    /// No user has the name.
    #[error("no user {name}")]
    NoUser {
        /// The name as it was given.
        name: String,
    },
    /// A new password is shorter than 12 characters, counted in UTF-16 code units.
    #[error("a password needs at least 12 characters")]
    PasswordTooShort,
    /// A key name isn't 1-64 characters after trimming, counted in UTF-16 code units.
    #[error("a key name is 1-64 characters")]
    InvalidKeyName,
    /// No API key has the id.
    #[error("no API key {id}")]
    NoApiKey {
        /// The id.
        id: i64,
    },
    /// The operating system's random source failed.
    #[error("cannot read random bytes from the operating system: {message}")]
    Random {
        /// The failure.
        message: String,
    },
    /// The argon2 implementation refused the parameters, which only a bug can cause.
    #[error("cannot hash the password: {message}")]
    Hashing {
        /// The complaint.
        message: String,
    },
    /// A stored password hash isn't an argon2id PHC string. spareline-old never wrote one.
    #[error("the stored password hash is not an argon2id PHC string")]
    UnreadableHash,
    /// The database failed.
    #[error(transparent)]
    Database(#[from] DatabaseError),
}

impl AccountError {
    /// Whether this is one of spareline-old's `AuthError`s, whose message is meant for the
    /// person who made the request, rather than an internal failure.
    #[must_use]
    pub fn is_user_error(&self) -> bool {
        matches!(
            self,
            Self::InvalidUserName
                | Self::UserExists { .. }
                | Self::NoUser { .. }
                | Self::PasswordTooShort
                | Self::InvalidKeyName
                | Self::NoApiKey { .. }
        )
    }
}

impl From<rusqlite::Error> for AccountError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Database(DatabaseError::Sqlite(error))
    }
}
