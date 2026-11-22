//! When the probes run.

use std::time::Duration;

use crate::Timestamp;
use crate::probe::{PhoneHealth, PhoneIdentity};

/// The delays before each retry of the first identity probe after a phone comes online, while
/// its telephony stack may still be starting: 5 s, 15 s, then 45 s. After the last retry fails,
/// the probe gives up until the next trigger.
pub const IDENTITY_RETRY_DELAYS: [Duration; 3] = [
    Duration::from_secs(5),
    Duration::from_secs(15),
    Duration::from_secs(45),
];

/// How often the identity is read again even when no SIM state changes: 30 minutes.
pub const IDENTITY_INTERVAL: Duration = Duration::from_mins(30);

/// The shell timeout of each identity and health script: 20 s.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(20);

/// Whether a periodic health probe should read the identity too: there is none yet, the SIM
/// states differ from the identity's (each slot's state, a missing one as empty, joined with
/// commas), or [`IDENTITY_INTERVAL`] has passed since `identity_at`. An identity without a time
/// is due only for the first two reasons.
#[must_use]
pub fn identity_due(
    identity: Option<&PhoneIdentity>,
    identity_at: Option<Timestamp>,
    health: &PhoneHealth,
    now: Timestamp,
) -> bool {
    let Some(identity) = identity else {
        return true;
    };
    let identity_sims: Vec<&str> = identity
        .slots
        .iter()
        .map(|slot| slot.sim_state.as_deref().unwrap_or(""))
        .collect();
    identity_sims.join(",") != health.sim_states.join(",")
        || identity_at.is_some_and(|at| at.plus(IDENTITY_INTERVAL) <= now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::{self, SlotPresence, SlotState};

    fn identity(states: &[Option<&str>]) -> PhoneIdentity {
        let mut identity = probe::parse_identity("").into_identity();
        identity.slots = states
            .iter()
            .zip(0..)
            .map(|(state, slot)| SlotState {
                slot,
                state: SlotPresence::Unknown,
                sim_state: state.map(str::to_owned),
            })
            .collect();
        identity
    }

    fn health(states: &[&str]) -> PhoneHealth {
        PhoneHealth {
            battery: None,
            sim_states: states.iter().map(|state| (*state).to_owned()).collect(),
            radios: Vec::new(),
        }
    }

    #[test]
    fn due_without_an_identity_or_on_a_sim_change() {
        let now = Timestamp::from_millis(10_000_000);
        let at = Some(now);
        assert!(
            identity_due(None, None, &health(&[]), now),
            "no identity yet"
        );
        let current = identity(&[Some("LOADED"), Some("ABSENT")]);
        assert!(
            !identity_due(Some(&current), at, &health(&["LOADED", "ABSENT"]), now),
            "nothing changed"
        );
        assert!(
            identity_due(Some(&current), at, &health(&["ABSENT", "ABSENT"]), now),
            "a SIM state changed"
        );
        let missing = identity(&[None, Some("ABSENT")]);
        assert!(
            !identity_due(Some(&missing), at, &health(&["", "ABSENT"]), now),
            "a missing state compares as empty"
        );
    }

    #[test]
    fn due_once_the_interval_has_passed() {
        let at = Timestamp::from_millis(0);
        let current = identity(&[Some("LOADED")]);
        let sims = health(&["LOADED"]);
        let just_before = at.plus(IDENTITY_INTERVAL).minus(Duration::from_millis(1));
        assert!(
            !identity_due(Some(&current), Some(at), &sims, just_before),
            "not yet"
        );
        assert!(
            identity_due(Some(&current), Some(at), &sims, at.plus(IDENTITY_INTERVAL)),
            "exactly the interval"
        );
        assert!(
            !identity_due(Some(&current), None, &sims, at.plus(IDENTITY_INTERVAL)),
            "no identity time"
        );
    }
}
