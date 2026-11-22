//! The monitor's readings: mobile data use from `dumpsys netstats`, and SIM changes.

mod netstats;
mod sim_usage;
mod sim_watch;

pub use netstats::{NetBucket, NetEntry, NetSection, parse_netstats};
pub use sim_usage::{
    DATA_WINDOW_SECONDS, PHONE_UID, SimDataUsage, SimUsage, UidBytes, internet_usage,
};
pub use sim_watch::{
    SIM_LOG_COMMAND, SIM_LOG_PATTERN, SIM_STATE_COMMAND, SimChange, SimChangeKind, SimLogScanner,
    SimSignal, SimStatePoll, diff_slots, slot_map,
};
