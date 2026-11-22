//! spareline's SQLite database: the migrations, which stay byte-compatible with the databases
//! spareline-old wrote, and the stores for accounts, inventory, device events, texts, health
//! and screen audits, as a synchronous library over one connection.
//!
//! Open it with [`Database::open`], then borrow a store from it, such as
//! [`Database::accounts`]. Every time it writes is the 24-character form of
//! [`Timestamp`](spareline_core::Timestamp), from a time the caller passes, and every query is
//! spareline-old's, so the database reads and sorts the same under either build.

mod account_error;
mod accounts;
mod api_key;
mod backup;
mod database;
mod database_error;
mod js_text;
mod migration;
mod node_message;
mod password_hash;
mod passwords;
mod principal;
mod resolve;
mod token;
mod user;

#[cfg(test)]
mod test_clock;
#[cfg(test)]
mod test_dir;
#[cfg(test)]
mod test_passwords;

pub use account_error::AccountError;
pub use accounts::{Accounts, KEY_PREFIX, SESSION_LIFETIME};
pub use api_key::{ApiKeyView, KeyScope, NewApiKey};
pub use backup::{BackupError, BackupSaved, IntegrityOk, backup, check};
pub use database::Database;
pub use database_error::DatabaseError;
pub use migration::AppliedMigration;
pub use password_hash::PasswordHash;
pub use passwords::{MIN_PASSWORD_LENGTH, Passwords};
pub use principal::{KeyPrincipal, Principal, SessionLookup, SessionPrincipal};
pub use user::{LoginCandidate, NewUser, User};
