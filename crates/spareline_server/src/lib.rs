//! The running spareline service and the commands that run beside it: the settings, the device
//! smoke, the container health check, the account commands and the policy check.

mod account_commands;
pub mod api_docs;
mod api_error;
mod buses;
mod clock;
mod config;
pub mod credentials;
mod db_handle;
mod devices;
pub mod embedded;
mod event_log;
#[cfg(test)]
mod fake_adb;
pub mod healthcheck;
mod login_limiter;
mod policy_check;
mod shell_text;
pub mod smoke;
mod sms_ingest;
pub mod stream_cursor;
mod web_files;

pub use account_commands::{
    CommandOutcome, KeysCommand, UsersCommand, parse_key_id, parse_scope, run_keys, run_users,
};
pub use api_error::ApiError;
pub use buses::{AuthChange, Buses};
pub use clock::Clock;
pub use config::{Config, EnvError, EnvIssue, SmokeSettings};
pub use db_handle::DbHandle;
pub use devices::{
    AdbStatus, DeviceError, DeviceManager, DeviceOptions, Devices, LivePhone, MAX_BACKOFF,
    PUSH_TIMEOUT, PhoneEvent, PhoneShell, PhoneStream,
};
pub use event_log::{EventLog, LogLine};
pub use login_limiter::LoginLimiter;
pub use policy_check::{PolicyCheckError, policy_check, read_profile};
pub use sms_ingest::{
    IngestError, RECONCILE_INTERVAL, SmsIngest, SmsIngestOptions, TRIGGER_INTERVAL,
};
pub use web_files::WebFiles;
