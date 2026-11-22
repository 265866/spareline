//! The running spareline service and the commands that run beside it: the settings, the device
//! smoke and the container health check.

pub mod api_docs;
mod api_error;
mod config;
pub mod credentials;
pub mod embedded;
pub mod healthcheck;
mod login_limiter;
pub mod smoke;
pub mod stream_cursor;
mod web_files;

pub use api_error::ApiError;
pub use config::{Config, EnvError, EnvIssue, SmokeSettings};
pub use login_limiter::LoginLimiter;
pub use web_files::WebFiles;
