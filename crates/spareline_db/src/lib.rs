//! spareline's SQLite database: the migrations, which stay byte-compatible with the databases
//! spareline-old wrote, and the stores for accounts, inventory, device events, texts, health
//! and screen audits, as a synchronous library over one connection.
//!
//! Open it with [`Database::open`], then borrow a store from it, such as
//! [`Database::accounts`]. Every time it writes is the 24-character form of
//! [`Timestamp`](spareline_core::Timestamp), from a time the caller passes, and every query is
//! spareline-old's, so the database reads and sorts the same under either build.

mod backup;
mod database;
mod database_error;
mod migration;
mod node_message;
mod resolve;

#[cfg(test)]
mod test_clock;
#[cfg(test)]
mod test_dir;

pub use backup::{BackupError, BackupSaved, IntegrityOk, backup, check};
pub use database::Database;
pub use database_error::DatabaseError;
pub use migration::AppliedMigration;
