//! Each slot's service and signal, from `dumpsys telephony.registry`.

use std::collections::HashMap;

use serde::Serialize;

use crate::text::{self, Sign, Start};

/// One slot's radio, as the API's `Radio` serializes it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RadioState {
    /// The logical slot: the registry's `Phone Id`.
    pub slot: u32,
    /// Voice registration, from `mVoiceRegState`.
    pub voice: ServiceState,
    /// Data registration, from `mDataRegState`.
    pub data: ServiceState,
    /// The network's long name, else its short name.
    pub operator: Option<String>,
    /// `mIsEmergencyOnly=true`.
    pub emergency_only: bool,
    /// `SignalStrength`'s level for its primary radio technology, 0 (none) to 4 (great).
    pub signal_level: i64,
    /// The primary radio technology in upper case, such as `LTE` or `NR`. A slot without a SIM
    /// still names one, with every value unset, and then this is `None`.
    pub signal_type: Option<String>,
    /// The primary technology's power in dBm: `rsrp` for LTE, `ssRsrp` for NR, `rscp` for WCDMA
    /// and TD-SCDMA, `rssi` for GSM, `cdmaDbm` for CDMA.
    pub signal_dbm: Option<i64>,
    /// `mIsDataEnabled`, when it reads `true` or `false`.
    pub data_enabled: Option<bool>,
    /// Whether `mDataConnectionState` is `DATA_CONNECTED` (2). A negative state, such as the -1
    /// of a slot without a SIM, or a missing one, is `None`.
    pub data_connected: Option<bool>,
}

/// `ServiceState`'s registration states, serialized as their snake-case names, such as
/// `in_service`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceState {
    /// 0.
    InService,
    /// 1.
    OutOfService,
    /// 2.
    EmergencyOnly,
    /// 3.
    PowerOff,
    /// Missing, or a value this doesn't name.
    Unknown,
}

/// `CellInfo.UNAVAILABLE`, which Android prints for a value it doesn't have.
const UNAVAILABLE: i64 = 2_147_483_647;

/// Reads `dumpsys telephony.registry`, reduced by the health probe to the `Phone Id=N` headers and
/// the `mName=value` lines it reads. Each header starts a slot; lines before the first header are
/// skipped, and a field seen twice in a slot keeps its first value.
#[must_use]
pub fn parse_registry(output: &str) -> Vec<RadioState> {
    let mut states = Vec::new();
    let mut current: Option<SlotFields<'_>> = None;
    for line in text::lines(output) {
        if let Some(slot) = phone_id(line) {
            if let Some(finished) = current.take() {
                states.push(finished.radio_state());
            }
            current = Some(SlotFields {
                slot,
                fields: HashMap::new(),
            });
            continue;
        }
        if let Some(slot) = current.as_mut()
            && let Some(field) = registry_field(line)
        {
            slot.fields.entry(field.name).or_insert(field.value);
        }
    }
    if let Some(finished) = current {
        states.push(finished.radio_state());
    }
    states
}

/// The `mName=value` fields of one slot.
struct SlotFields<'output> {
    slot: u32,
    fields: HashMap<&'output str, &'output str>,
}

impl SlotFields<'_> {
    fn radio_state(&self) -> RadioState {
        let service = self.fields.get("mServiceState").copied().unwrap_or("");
        let signal = self.fields.get("mSignalStrength").copied().unwrap_or("");
        let operator = operator_name(service, "mOperatorAlphaLong=")
            .or_else(|| operator_name(service, "mOperatorAlphaShort="));
        let primary = PrimarySignal::read(signal);
        let connection = self
            .fields
            .get("mDataConnectionState")
            .and_then(|value| text::parse_int(text::trim(value)));
        RadioState {
            slot: self.slot,
            voice: registration(service, "mVoiceRegState="),
            data: registration(service, "mDataRegState="),
            operator,
            emergency_only: service.contains("mIsEmergencyOnly=true"),
            signal_level: primary.level,
            signal_type: primary.kind,
            signal_dbm: primary.dbm,
            data_enabled: match self.fields.get("mIsDataEnabled").copied() {
                Some("true") => Some(true),
                Some("false") => Some(false),
                _ => None,
            },
            data_connected: connection
                .filter(|&state| state >= 0)
                .map(|state| state == 2),
        }
    }
}

/// The slot of a `Phone Id=N` header, as `/^\s*Phone Id=(\d+)\s*$/` matches it.
fn phone_id(line: &str) -> Option<u32> {
    let digits = text::trim(line).strip_prefix("Phone Id=")?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// One `mName=value` line.
struct RegistryField<'line> {
    name: &'line str,
    value: &'line str,
}

/// `/^\s+(m[A-Za-z]+)=(.*)$/`: indented, a name of `m` and letters, then a value that runs to the
/// end of the line.
fn registry_field(line: &str) -> Option<RegistryField<'_>> {
    let rest = text::trim_start(line);
    if rest.len() == line.len() || !rest.starts_with('m') {
        return None;
    }
    let length = rest
        .find(|c: char| !c.is_ascii_alphabetic())
        .unwrap_or(rest.len());
    if length < 2 {
        return None;
    }
    let value = rest[length..].strip_prefix('=')?;
    if value.contains(text::is_line_terminator) {
        return None;
    }
    Some(RegistryField {
        name: &rest[..length],
        value,
    })
}

/// The value after the first `prefix` up to the next comma, trimmed, unless it is empty or
/// `null`.
fn operator_name(service: &str, prefix: &str) -> Option<String> {
    let start = service.find(prefix)? + prefix.len();
    let value = &service[start..];
    let value = text::trim(&value[..value.find(',').unwrap_or(value.len())]);
    (!value.is_empty() && value != "null").then(|| value.to_owned())
}

fn registration(service: &str, prefix: &str) -> ServiceState {
    match text::number_after(service, prefix, Sign::Unsigned, Start::Anywhere) {
        Some("0") => ServiceState::InService,
        Some("1") => ServiceState::OutOfService,
        Some("2") => ServiceState::EmergencyOnly,
        Some("3") => ServiceState::PowerOff,
        _ => ServiceState::Unknown,
    }
}

/// The level, technology and power of `SignalStrength`'s primary technology.
struct PrimarySignal {
    level: i64,
    kind: Option<String>,
    dbm: Option<i64>,
}

impl PrimarySignal {
    fn read(signal: &str) -> Self {
        const PRIMARY: &str = "primary=CellSignalStrength";
        let kind = text::occurrences(signal, PRIMARY).find_map(|index| {
            let after = &signal[index + PRIMARY.len()..];
            let end = after.find(|c| !text::is_word(c)).unwrap_or(after.len());
            (end > 0).then(|| &after[..end])
        });
        let Some(kind) = kind else {
            return Self {
                level: 0,
                kind: None,
                dbm: None,
            };
        };
        let Some(start) = signal.find(&format!("m{kind}=CellSignalStrength{kind}")) else {
            return Self {
                level: 0,
                kind: Some(kind.to_owned()),
                dbm: None,
            };
        };
        let rest = &signal[start..];
        let segment = &rest[..next_signal_part(rest).unwrap_or(rest.len())];
        let level = signal_level(segment).unwrap_or(0);
        let dbm_field = match kind {
            "Lte" => Some("rsrp="),
            "Nr" => Some("ssRsrp = "),
            "Wcdma" | "Tdscdma" => Some("rscp="),
            "Gsm" => Some("rssi="),
            "Cdma" => Some("cdmaDbm="),
            _ => None,
        };
        let dbm = dbm_field
            .and_then(|field| text::number_after(segment, field, Sign::Signed, Start::WordStart))
            .and_then(text::parse_int)
            .filter(|raw| raw.abs() != UNAVAILABLE);
        if level == 0 && dbm.is_none() {
            return Self {
                level,
                kind: None,
                dbm,
            };
        }
        Self {
            level,
            kind: Some(kind.to_ascii_uppercase()),
            dbm,
        }
    }
}

/// Where the next technology or the `primary=` field starts, as
/// `/,m[A-Z][a-z]+=CellSignalStrength|,primary=/` finds it.
fn next_signal_part(rest: &str) -> Option<usize> {
    rest.match_indices(',')
        .map(|(index, _)| index)
        .find(|&index| {
            let after = &rest[index + 1..];
            if after.starts_with("primary=") {
                return true;
            }
            let Some(name) = after.strip_prefix('m') else {
                return false;
            };
            if !name.starts_with(|c: char| c.is_ascii_uppercase()) {
                return false;
            }
            let lower = name[1..]
                .find(|c: char| !c.is_ascii_lowercase())
                .unwrap_or(name.len() - 1);
            lower > 0 && name[1 + lower..].starts_with("=CellSignalStrength")
        })
}

/// The first `/\b(?:mLevel|level) ?= ?(\d+)/` in the segment.
fn signal_level(segment: &str) -> Option<i64> {
    segment.char_indices().find_map(|(index, _)| {
        if !text::word_boundary_before(segment, index) {
            return None;
        }
        let rest = &segment[index..];
        let after = rest
            .strip_prefix("mLevel")
            .or_else(|| rest.strip_prefix("level"))?;
        let after = after.strip_prefix(' ').unwrap_or(after);
        let after = after.strip_prefix('=')?;
        let after = after.strip_prefix(' ').unwrap_or(after);
        text::parse_int(text::leading_number(after, Sign::Unsigned)?)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(fields: &str) -> RadioState {
        let mut states = parse_registry(&format!("  Phone Id=0\n{fields}"));
        assert_eq!(states.len(), 1, "one slot");
        states.remove(0)
    }

    #[test]
    fn lines_before_the_first_header_are_skipped() {
        assert_eq!(parse_registry("    mIsDataEnabled=true\n"), []);
        assert_eq!(parse_registry("Phone Id=x\n"), []);
        assert_eq!(parse_registry("Phone Id=3  \n")[0].slot, 3);
    }

    #[test]
    fn a_slot_without_fields() {
        assert_eq!(
            slot(""),
            RadioState {
                slot: 0,
                voice: ServiceState::Unknown,
                data: ServiceState::Unknown,
                operator: None,
                emergency_only: false,
                signal_level: 0,
                signal_type: None,
                signal_dbm: None,
                data_enabled: None,
                data_connected: None,
            }
        );
    }

    #[test]
    fn the_first_value_of_a_field_wins_and_fields_need_indenting() {
        let state =
            slot("    mIsDataEnabled=true\n    mIsDataEnabled=false\nmDataConnectionState=2\n");
        assert_eq!(state.data_enabled, Some(true));
        assert_eq!(state.data_connected, None);
        assert_eq!(slot("  mDataConnectionState=2").data_connected, Some(true));
        assert_eq!(slot("  mDataConnectionState=1").data_connected, Some(false));
        assert_eq!(slot("  mIsDataEnabled= true").data_enabled, None);
    }

    #[test]
    fn registration_codes() {
        let state = slot("  mServiceState={mVoiceRegState=2(EMERGENCY_ONLY), mDataRegState=09}");
        assert_eq!(state.voice, ServiceState::EmergencyOnly);
        assert_eq!(state.data, ServiceState::Unknown);
    }

    #[test]
    fn the_operator_falls_back_to_the_short_name() {
        let state = slot("  mServiceState={mOperatorAlphaLong= null , mOperatorAlphaShort=TMO, x}");
        assert_eq!(state.operator.as_deref(), Some("TMO"));
        let state = slot("  mServiceState={mOperatorAlphaLongRaw=Raw, mOperatorAlphaShort=, x=y}");
        assert_eq!(state.operator, None);
    }

    #[test]
    fn a_primary_technology_without_its_part_keeps_its_name_as_printed() {
        let state = slot(
            "  mSignalStrength=SignalStrength:{mGsm=CellSignalStrengthGsm: rssi=-90 mLevel=3,primary=CellSignalStrengthLte}",
        );
        assert_eq!(state.signal_type.as_deref(), Some("Lte"));
        assert_eq!(state.signal_level, 0);
    }

    #[test]
    fn gsm_reads_its_level_and_rssi() {
        let state = slot(
            "  mSignalStrength=SignalStrength:{mGsm=CellSignalStrengthGsm: rssi=-90 ber=99 mTa=1 mLevel=3,mLte=CellSignalStrengthLte: rsrp=-100 level=4,primary=CellSignalStrengthGsm}",
        );
        assert_eq!(state.signal_type.as_deref(), Some("GSM"));
        assert_eq!(state.signal_level, 3);
        assert_eq!(state.signal_dbm, Some(-90));
    }

    #[test]
    fn nr_reads_its_spaced_fields() {
        let state = slot(
            "  mSignalStrength=SignalStrength:{mNr=CellSignalStrengthNr:{ csiRsrp = -80 ssRsrp = -95 level = 2 },primary=CellSignalStrengthNr}",
        );
        assert_eq!(state.signal_type.as_deref(), Some("NR"));
        assert_eq!(state.signal_level, 2);
        assert_eq!(state.signal_dbm, Some(-95));
    }

    #[test]
    fn an_unavailable_power_with_a_level_keeps_the_technology() {
        let state = slot(
            "  mSignalStrength=SignalStrength:{mLte=CellSignalStrengthLte: rsrp=-2147483647 level=1,primary=CellSignalStrengthLte}",
        );
        assert_eq!(state.signal_type.as_deref(), Some("LTE"));
        assert_eq!(state.signal_level, 1);
        assert_eq!(state.signal_dbm, None);
    }

    #[test]
    fn signal_part_boundaries() {
        assert_eq!(next_signal_part("mLte=x,mNr=CellSignalStrengthNr"), Some(6));
        assert_eq!(
            next_signal_part("mLte=x,mNR=CellSignalStrength,primary="),
            Some(29)
        );
        assert_eq!(next_signal_part("mLte=x,mN=CellSignalStrength"), None);
    }

    #[test]
    fn levels_need_a_word_boundary() {
        assert_eq!(signal_level("xlevel=3 level = 2"), Some(2));
        assert_eq!(signal_level("level=  3"), None);
        assert_eq!(signal_level("_mLevel=1 mLevel =4"), Some(4));
    }
}
