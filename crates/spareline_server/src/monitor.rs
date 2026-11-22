//! The monitor: health aggregates and alerts for every phone, the SIM watch for the inventory,
//! and, with a policy profile, the drift check, Set up phone and the data watch.
//!
//! Every command that changes a setting goes through `spareline_core`'s `ApplyPlan`, whose radio
//! guard refuses anything that could turn a radio or mobile data on; the reads are fixed
//! read-only commands. Without a profile it changes no setting on a phone: it reads the SIM
//! state, the radio log and isub, and asks the phone manager for an identity refresh.

mod monitor_context;
mod monitor_error;
#[cfg_attr(
    dylint_lib = "trellis_lints",
    expect(
        file_named_after_item,
        reason = "Monitor's natural file is monitor/monitor.rs, which clippy's module_inception \
                  refuses, so it lives beside the module root under another name"
    )
)]
mod monitor_handle;
mod monitor_settings;
mod monitor_status;
mod phone_runtime;
mod screen_sessions;
mod work_tracker;

pub use monitor_context::MonitorContext;
pub use monitor_error::MonitorError;
pub use monitor_handle::Monitor;
pub use monitor_settings::MonitorSettings;
pub use monitor_status::MonitorStatus;
pub use screen_sessions::ScreenSessions;
