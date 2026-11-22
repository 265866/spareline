//! How the monitor is set up: the policy and its timings.

use std::time::Duration;

use spareline_core::monitor::Policy;

use crate::Config;

/// The monitor's policy and timings. [`MonitorSettings::default`] holds spareline-old's: no
/// policy, a check every 5 minutes, a health sample every minute, the SIM state polled every
/// 10 s, the SIM log reopened 30 s after it ends, and 1 s of quiet after a SIM signal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MonitorSettings {
    /// The rules from the policy profile, or `None` to check and change nothing on the phones.
    pub policy: Option<Policy>,
    /// How often each online phone gets a drift check, `POLICY_CHECK_INTERVAL_S`.
    pub check_interval: Duration,
    /// How often health is sampled and the alerts recomputed, `HEALTH_INTERVAL_S`.
    pub health_interval: Duration,
    /// How often each online phone's `gsm.sim.state` is read.
    pub sim_poll: Duration,
    /// How long after the SIM log stream ends it is opened again.
    pub sim_log_retry: Duration,
    /// Quiet time after a SIM signal before `dumpsys isub` is read again, so one swap's burst of
    /// log lines costs one read.
    pub sim_debounce: Duration,
}

impl MonitorSettings {
    /// The settings for `config` with `policy`, the profile `config.policy_profile` names.
    #[must_use]
    pub fn new(config: &Config, policy: Option<Policy>) -> Self {
        Self {
            policy,
            check_interval: config.policy_check_interval,
            health_interval: config.health_interval,
            ..Self::default()
        }
    }
}

impl Default for MonitorSettings {
    fn default() -> Self {
        Self {
            policy: None,
            check_interval: Duration::from_mins(5),
            health_interval: Duration::from_mins(1),
            sim_poll: Duration::from_secs(10),
            sim_log_retry: Duration::from_secs(30),
            sim_debounce: Duration::from_secs(1),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;

    #[test]
    fn the_timings_are_the_old_services_and_the_intervals_come_from_the_environment() {
        let config = Config::from_env(|name| match name {
            "POLICY_CHECK_INTERVAL_S" => Some(OsString::from("120")),
            "HEALTH_INTERVAL_S" => Some(OsString::from("30")),
            _ => None,
        })
        .unwrap();
        let settings = MonitorSettings::new(&config, None);
        assert_eq!(
            settings,
            MonitorSettings {
                policy: None,
                check_interval: Duration::from_mins(2),
                health_interval: Duration::from_secs(30),
                sim_poll: Duration::from_secs(10),
                sim_log_retry: Duration::from_secs(30),
                sim_debounce: Duration::from_secs(1),
            }
        );
        let defaults = Config::from_env(|_| None).unwrap();
        let settings = MonitorSettings::new(&defaults, None);
        assert_eq!(settings.check_interval, Duration::from_mins(5));
        assert_eq!(settings.health_interval, Duration::from_mins(1));
        assert_eq!(settings, MonitorSettings::default());
    }
}
