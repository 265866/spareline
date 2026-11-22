//! What the phone manager is started with.

use std::time::Duration;

use spareline_adb::AdbClient;

use crate::buses::Buses;
use crate::clock::Clock;
use crate::db_handle::DbHandle;
use crate::event_log::EventLog;

/// The longest wait between attempts to reach the adb server: 15 s.
pub const MAX_BACKOFF: Duration = Duration::from_secs(15);

/// What [`DeviceManager`](crate::DeviceManager) needs: the adb server, the database it records
/// phones in, the bus it publishes their events on, and its timing.
#[derive(Clone, Debug)]
pub struct DeviceOptions {
    /// The adb server.
    pub adb: AdbClient,
    /// The database.
    pub db: DbHandle,
    /// Where recorded device events are published.
    pub buses: Buses,
    /// The site a new phone row gets (`SITE`).
    pub site: String,
    /// The log.
    pub log: EventLog,
    /// The clock every recorded time is read from.
    pub clock: Clock,
    /// How often each online phone's health is probed (`HEALTH_INTERVAL_S`).
    pub health_interval: Duration,
    /// The delays before each retry of a phone's first identity probe after it comes online:
    /// [`IDENTITY_RETRY_DELAYS`](spareline_core::IDENTITY_RETRY_DELAYS) unless a test sets others.
    pub identity_retry: Vec<Duration>,
    /// The longest wait between attempts to reach the adb server: [`MAX_BACKOFF`] unless a test
    /// sets another.
    pub max_backoff: Duration,
}

impl DeviceOptions {
    /// The options spareline-old's server used, with the retry delays and backoff it set.
    #[must_use]
    pub fn new(
        adb: AdbClient,
        db: DbHandle,
        buses: Buses,
        site: String,
        log: EventLog,
        clock: Clock,
        health_interval: Duration,
    ) -> Self {
        Self {
            adb,
            db,
            buses,
            site,
            log,
            clock,
            health_interval,
            identity_retry: spareline_core::IDENTITY_RETRY_DELAYS.to_vec(),
            max_backoff: MAX_BACKOFF,
        }
    }
}
