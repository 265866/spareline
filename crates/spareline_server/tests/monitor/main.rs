//! The monitor's runtime against a B30 stand-in behind a stand-in adb server: the phone manager,
//! the database, the event bus and the monitor are real, and the phone answers with the B30's
//! recorded output, with the monitor's writes applied to the recording (see
//! `monitor/monitor_phone.rs` for why).

mod adb_stand_in;
mod monitor_phone;
mod monitor_runtime;
