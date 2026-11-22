//! The running spareline service and the commands that run beside it: the settings, the device
//! smoke, the container health check, the account commands and the policy check.

mod account_commands;
pub mod api_docs;
mod api_error;
mod config;
pub mod credentials;
pub mod embedded;
pub mod healthcheck;
mod login_limiter;
mod policy_check;
pub mod smoke;
pub mod stream_cursor;
mod web_files;

pub use account_commands::{CommandOutcome, KeysCommand, UsersCommand, run_keys, run_users};
pub use api_error::ApiError;
pub use config::{Config, EnvError, EnvIssue, SmokeSettings};
pub use login_limiter::LoginLimiter;
pub use policy_check::{PolicyCheckError, policy_check, read_profile};
pub use web_files::WebFiles;
