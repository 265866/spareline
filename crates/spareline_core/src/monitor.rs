//! The phone policy and the health monitor: the profile and its rules, the batched read and the
//! enforce script, the radio guard, mobile data use from `dumpsys netstats`, SIM changes, alerts
//! and the 5-minute health buckets.
//!
//! Nothing here touches a phone, a clock or the database: the server runs the commands these
//! build, passes in the output and the times, and writes the values these return.

mod alert;
mod apply_plan;
mod check_report;
mod check_run;
mod health_tracker;
mod iso_time;
mod js_regex;
mod json_value;
mod lock_screen_notice;
mod lock_wallpaper;
mod monitor_event;
mod namespace;
mod netstats;
mod policy;
mod profile;
mod profile_schema;
mod radio_hazard;
mod rule;
mod rule_result;
mod severity;
mod sim_usage;
mod sim_watch;
mod snapshot;

pub use alert::{
    Alert, AlertChanges, AlertKind, AlertSet, DataOffCause, DataOffFailure, PolicyAlertSource,
};
pub use apply_plan::{ApplyPlan, CommandResult, RefusedCommand, Upload};
pub use check_report::{
    Busy, CheckReason, CheckReport, FailedCommand, MobileBytes, PhonePolicy, ProvisionReport,
    ProvisionStep, Revert,
};
pub use check_run::{
    CheckFinish, CheckFinished, DataOffFailureNote, EnforceBatch, RoutineRevert, data_off_cause,
    inserted_sims,
};
pub use health_tracker::{
    BUCKET, COOL_BATTERY_DECI_C, HOT_BATTERY_DECI_C, HealthRow, HealthSampleOutput, HealthTracker,
    IN_SERVICE_SAMPLES_TO_CLEAR, OFFLINE_ALERT, OUT_OF_SERVICE_ALERT, PRUNE_INTERVAL, PhoneSample,
    RETENTION,
};
pub use lock_screen_notice::{
    LOCK_SCREEN_DEX_PHONE_PATH, LOCK_SCREEN_DEX_SHA256, LockScreenNotice, verify_lock_screen_dex,
};
pub use lock_wallpaper::{LOCK_WALLPAPER_READ, LockWallpaper};
pub use monitor_event::MonitorEvent;
pub use namespace::Namespace;
pub use netstats::{NetBucket, NetEntry, NetSection, parse_netstats};
pub use policy::{
    ENFORCE_TIMEOUT, IsubUnreadable, POLICY_READ_TIMEOUT, PRESET_SUB_IDS, Policy, PolicyReadError,
    ProfileSummary, check_isub, data_off_commands, preset_data_off_commands,
};
pub use profile::{Profile, ProfileError, RadiosOff, ScreenSettings, profile_name};
pub use radio_hazard::radio_hazard;
pub use rule::{
    CheckContext, Outcome, PhoneFile, Rule, RuleSelection, SMS_ROLE, SettingKey, log_buffer_command,
};
pub use rule_result::{PlanStep, RuleResult, evaluate, plan};
pub use severity::Severity;
pub use sim_usage::{
    DATA_WINDOW_SECONDS, PHONE_UID, SimDataUsage, SimUsage, UidBytes, internet_usage,
};
pub use sim_watch::{
    SIM_LOG_COMMAND, SIM_LOG_PATTERN, SIM_STATE_COMMAND, SimChange, SimChangeKind, SimLogScanner,
    SimSignal, SimStatePoll, diff_slots, slot_map,
};
pub use snapshot::{ActiveSub, MMS_ALWAYS_ALLOWED, SlotDataEnabled, Snapshot};
