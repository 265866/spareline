//! The parts of the service the monitor works through.

use crate::{Buses, Clock, DbHandle, Devices, EventLog};

/// What the monitor works through: the phones, the database, the event bus, the clock and the
/// log.
#[derive(Clone, Debug)]
pub struct MonitorContext {
    /// The phones. Every command the monitor sends goes through it.
    pub devices: Devices,
    /// The database, for device events, health buckets and the last Set up phone run.
    pub db: DbHandle,
    /// Where the device events the monitor records are published.
    pub buses: Buses,
    /// The time every report, alert and event is stamped with.
    pub clock: Clock,
    /// The service log.
    pub log: EventLog,
}
