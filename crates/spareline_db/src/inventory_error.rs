//! The error inventory edits return.

use thiserror::Error;

use crate::database_error::DatabaseError;

/// A failed inventory edit. The messages of the not-found and conflict variants reach the API
/// as they are.
#[derive(Debug, Error)]
#[must_use]
pub enum InventoryError {
    /// No phone has the serial.
    #[error("no phone {serial}")]
    NoPhone {
        /// The serial.
        serial: String,
    },
    /// No SIM has the id.
    #[error("no SIM {id}")]
    NoSim {
        /// The id.
        id: i64,
    },
    /// No number has the id.
    #[error("no number {id}")]
    NoNumber {
        /// The id.
        id: i64,
    },
    /// Another number already has the E.164 value.
    #[error("{e164} already belongs to number {owner}")]
    Conflict {
        /// The value.
        e164: String,
        /// The number that has it.
        owner: i64,
    },
    /// The database failed.
    #[error(transparent)]
    Database(#[from] DatabaseError),
}

impl From<rusqlite::Error> for InventoryError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Database(DatabaseError::Sqlite(error))
    }
}
