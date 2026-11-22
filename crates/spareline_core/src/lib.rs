//! The logic behind `spareline`, apart from its binary so the binary's command-line and
//! runtime dependencies stay out of it.

mod android;
mod identifiers;
mod probe;
mod sms;
mod text;
mod timestamp;
mod version_line;

pub use android::{
    Battery, BatteryHealth, BatteryStatus, Isub, ParcelString, PowerSource, RadioState, Section,
    Sections, ServiceState, SubscriptionRecord, parse_battery, parse_isub, parse_key_values,
    parse_parcel_string, parse_registry, parse_sim_states, section_marker, split_sections,
};
pub use identifiers::{
    SubInfoCodes, SubInfoLookup, is_iccid, is_imei, sub_info_call, sub_info_codes, to_e164,
};
pub use probe::{
    IDENTITY_INTERVAL, IDENTITY_RETRY_DELAYS, IdentityProbe, PROBE_TIMEOUT, PhoneHealth,
    PhoneIdentity, ProbeError, SimIdentity, SlotPresence, SlotState, check_probe_exit,
    health_command, identity_command, identity_due, parse_health, parse_identity,
};
pub use sms::{
    ContentQueryError, ContentRow, ContentRows, HexError, INBOX_KEY_COLUMNS, ISMS_TRIGGER_COMMAND,
    InboxKey, IsmsActivity, MMS_ADDR_COLUMNS, MMS_COLUMNS, MMS_PART_COLUMNS, MmsPart,
    MmsPartSignature, MmsSignature, PartReadError, PartRef, Provider, ProviderMessage,
    ProviderQuery, ProviderRowError, SMS_COLUMNS, check_content_output, check_part_read,
    decode_hex, inbox_keys, mms_message, mms_part, mms_signature, parse_content_rows, parse_isms,
    read_part_command, sms_message,
};
pub use timestamp::Timestamp;
pub use version_line::version_line;
