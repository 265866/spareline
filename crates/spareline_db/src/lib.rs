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
mod attachment_store;
mod backup;
mod control_sessions;
mod database;
mod database_error;
mod device_event;
mod device_events;
mod field_patch;
mod health_store;
mod identity_reading;
mod inventory;
mod inventory_error;
mod inventory_sync;
mod js_text;
mod message;
mod message_store;
mod migration;
mod number_record;
mod number_summary;
mod password_hash;
mod passwords;
mod phone_record;
mod phone_state;
mod phone_state_update;
mod principal;
mod resolve;
mod sim_record;
mod sim_summary;
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
pub use attachment_store::{AttachmentStore, SavedAttachment};
pub use backup::{BackupError, BackupSaved, IntegrityOk, backup, check};
pub use control_sessions::{ControlCounts, ControlSessions};
pub use database::Database;
pub use database_error::DatabaseError;
pub use device_event::{DeviceEvent, LastProvision, NewEvent};
pub use device_events::DeviceEvents;
pub use field_patch::FieldPatch;
pub use health_store::HealthStore;
pub use identity_reading::{IdentityReading, SimReading, SlotContents, SlotReading};
pub use inventory::Inventory;
pub use inventory_error::InventoryError;
pub use message::{
    AttachmentFile, AttachmentView, Attribution, Message, MessageKey, MessageNumber, MessagePage,
    MessageScope, NewAttachment, NewMessage, Paging, Provider,
};
pub use message_store::MessageStore;
pub use migration::AppliedMigration;
pub use number_record::{LastMessage, NumberPatch, NumberPhone, NumberRecord};
pub use number_summary::{NumberSource, NumberSummary};
pub use password_hash::PasswordHash;
pub use passwords::{MIN_PASSWORD_LENGTH, Passwords, max_concurrent_hashes};
pub use phone_record::{PhonePatch, PhoneRecord, PlacementRecord};
pub use phone_state::{PhoneState, PhoneStatus};
pub use phone_state_update::{PhoneStateUpdate, RecordedState};
pub use principal::{KeyPrincipal, Principal, SessionLookup, SessionPrincipal};
pub use sim_record::{LastPlacement, SimPatch, SimPlacement, SimRecord, SimStatus};
pub use sim_summary::SimSummary;
pub use user::{LoginCandidate, NewUser, User};
