//! How long the screen relay waits for each step, spareline-old's values by default.

use std::time::Duration;

/// The relay's timings. The service runs on [`ScreenTimings::default`], spareline-old's
/// values; tests shorten them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ScreenTimings {
    /// How long a session keeps running after its last viewer leaves: 30 s.
    pub(crate) idle: Duration,
    /// The waits before each restart of a session that died with viewers watching: 500 ms, 2 s
    /// and 5 s. When they run out, the viewers are closed with 4002.
    pub(crate) restart_delays: Vec<Duration>,
    /// How long a session must have run for its next failure to start the restart budget over:
    /// 10 s.
    pub(crate) healthy_after: Duration,
    /// How often viewers get a `stats` frame: 2 s.
    pub(crate) stats_interval: Duration,
    /// How often a watched phone is checked to be awake: 5 s.
    pub(crate) wake_check: Duration,
    /// How long a start keeps trying the video socket: 10 s.
    pub(crate) connect_timeout: Duration,
}

impl Default for ScreenTimings {
    fn default() -> Self {
        Self {
            idle: Duration::from_secs(30),
            restart_delays: vec![
                Duration::from_millis(500),
                Duration::from_secs(2),
                Duration::from_secs(5),
            ],
            healthy_after: Duration::from_secs(10),
            stats_interval: Duration::from_secs(2),
            wake_check: Duration::from_secs(5),
            connect_timeout: Duration::from_secs(10),
        }
    }
}
