//! The identity and health probes: the exact shell scripts the server runs on a phone, and the
//! results they give from the scripts' output. Running the scripts is the server's part.

mod phone_health;
mod phone_identity;
mod probe_error;
mod schedule;

pub use phone_health::{PhoneHealth, health_command, parse_health};
pub use phone_identity::{
    IdentityProbe, PhoneIdentity, SimIdentity, SlotPresence, SlotState, identity_command,
    parse_identity,
};
pub use probe_error::{ProbeError, check_probe_exit};
pub use schedule::{IDENTITY_INTERVAL, IDENTITY_RETRY_DELAYS, PROBE_TIMEOUT, identity_due};
