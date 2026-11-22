//! Reading inbound texts from the phone: the `dumpsys isms` trigger, the `content` commands that
//! query the SMS and MMS providers, and the messages their rows make.

mod content_check;
mod content_rows;
mod inbox_key;
mod isms_activity;
mod mms_part;
mod provider_message;
mod provider_query;

pub use content_check::{ContentQueryError, PartReadError, check_content_output, check_part_read};
pub use content_rows::{ContentRow, ContentRows, HexError, decode_hex, parse_content_rows};
pub use inbox_key::{InboxKey, inbox_keys};
pub use isms_activity::{ISMS_TRIGGER_COMMAND, IsmsActivity, parse_isms};
pub use mms_part::{MmsPart, MmsPartSignature, MmsSignature, mms_part, mms_signature};
pub use provider_message::{PartRef, ProviderMessage, ProviderRowError, mms_message, sms_message};
pub use provider_query::{
    INBOX_KEY_COLUMNS, MMS_ADDR_COLUMNS, MMS_COLUMNS, MMS_PART_COLUMNS, Provider, ProviderQuery,
    SMS_COLUMNS, read_part_command,
};
