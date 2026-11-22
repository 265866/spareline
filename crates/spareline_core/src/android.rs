//! Parsers for the text Android's shell tools print. Each takes the exact output of one command
//! and is tested against output recorded from real phones in `tests/fixtures/`.

mod battery;
mod isub;
mod parcel_string;
mod registry;
mod sections;

pub use battery::{Battery, BatteryHealth, BatteryStatus, PowerSource, parse_battery};
pub use isub::{Isub, SubscriptionRecord, parse_isub};
pub use parcel_string::{ParcelString, parse_parcel_string};
pub use registry::{RadioState, ServiceState, parse_registry};
pub use sections::{
    Section, Sections, parse_key_values, parse_sim_states, section_marker, split_sections,
};
