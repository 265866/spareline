//! The logic behind `spareline`, apart from its binary so the binary's command-line and
//! runtime dependencies stay out of it.

mod android;
mod text;
mod timestamp;
mod version_line;

pub use android::{
    Battery, BatteryHealth, BatteryStatus, Isub, ParcelString, PowerSource, RadioState, Section,
    Sections, ServiceState, SubscriptionRecord, parse_battery, parse_isub, parse_key_values,
    parse_parcel_string, parse_registry, parse_sim_states, section_marker, split_sections,
};
pub use timestamp::Timestamp;
pub use version_line::version_line;
