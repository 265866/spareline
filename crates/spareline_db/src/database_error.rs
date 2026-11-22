//! The error a database call returns when SQLite, the file system or a stored JSON column fails.

use std::io;

use thiserror::Error;

/// A failed database call: SQLite refused a statement, a file or directory couldn't be created,
/// read or written, or a stored JSON column didn't parse.
#[derive(Debug, Error)]
#[must_use]
pub enum DatabaseError {
    /// SQLite failed.
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    /// A file or directory couldn't be created, read or written.
    #[error("{action}: {source}")]
    Io {
        /// What was being done, with the path, such as `cannot create /app/data`.
        action: String,
        /// The failure the operating system reported.
        #[source]
        source: io::Error,
    },
    /// A value couldn't be written as JSON, or a stored JSON column didn't parse.
    #[error("{what}: {source}")]
    Json {
        /// Which value, such as `detail_json of device event 12`.
        what: String,
        /// The parser's or serializer's complaint.
        #[source]
        source: serde_json::Error,
    },
    /// A migration file name or the order of the migrations is wrong.
    #[error("{message}")]
    Migrations {
        /// The complaint, as spareline-old worded it.
        message: String,
    },
    /// A path isn't valid UTF-8, which SQLite needs for `VACUUM INTO`.
    #[error("path is not valid UTF-8: {path}")]
    NonUtf8Path {
        /// The path, with invalid sequences replaced.
        path: String,
    },
}

impl DatabaseError {
    /// An I/O failure while doing `action`.
    pub(crate) fn io(action: String, source: io::Error) -> Self {
        Self::Io { action, source }
    }

    /// A JSON failure for the value `what` names.
    pub(crate) fn json(what: String, source: serde_json::Error) -> Self {
        Self::Json { what, source }
    }
}
