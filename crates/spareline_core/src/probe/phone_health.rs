//! The health probe: battery, SIM card states, and each slot's service and signal.

use crate::android::{self, Battery, RadioState};

/// The health script, in sections `battery`, `simstate` and `registry`. `telephony.registry` is
/// about 400 KB on the B30, almost all of it history, so `grep` keeps only the lines read here.
/// It is
///
/// ```text
/// printf '\n@@sl:battery\n'; dumpsys battery; printf '\n@@sl:simstate\n'; getprop gsm.sim.state; printf '\n@@sl:registry\n'; dumpsys telephony.registry | grep -E '^  Phone Id=|^    m(ServiceState|SignalStrength|IsDataEnabled|DataConnectionState|RadioPowerState)='
/// ```
#[must_use]
pub fn health_command() -> String {
    [
        android::section_marker("battery"),
        "dumpsys battery".to_owned(),
        android::section_marker("simstate"),
        "getprop gsm.sim.state".to_owned(),
        android::section_marker("registry"),
        "dumpsys telephony.registry | grep -E '^  Phone Id=|^    \
         m(ServiceState|SignalStrength|IsDataEnabled|DataConnectionState|RadioPowerState)='"
            .to_owned(),
    ]
    .join("; ")
}

/// A phone's health, as of its last health probe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhoneHealth {
    /// The battery, when `dumpsys battery` could be read.
    pub battery: Option<Battery>,
    /// Each slot's SIM state from `gsm.sim.state`, by slot.
    pub sim_states: Vec<String>,
    /// Each slot's radio, in the registry's order.
    pub radios: Vec<RadioState>,
}

/// Reads the output of [`health_command`]. A missing section reads as empty.
#[must_use]
pub fn parse_health(stdout: &str) -> PhoneHealth {
    let sections = android::split_sections(stdout);
    let section = |name: &str| sections.get(name).unwrap_or("");
    PhoneHealth {
        battery: android::parse_battery(section("battery")),
        sim_states: android::parse_sim_states(section("simstate")),
        radios: android::parse_registry(section("registry")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_output_is_an_empty_health() {
        assert_eq!(
            parse_health(""),
            PhoneHealth {
                battery: None,
                sim_states: Vec::new(),
                radios: Vec::new(),
            }
        );
    }
}
