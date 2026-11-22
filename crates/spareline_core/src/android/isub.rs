//! The subscription service's state, from `dumpsys isub`.

use std::collections::{BTreeMap, HashMap};

use crate::text::{self, Sign, Start};

/// What `dumpsys isub` reports on Android 14 (`SubscriptionManagerService`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Isub {
    /// `Active modem count=N`: how many SIM slots the modem runs.
    pub active_modem_count: Option<i64>,
    /// The sub id in each logical slot, from `Logical SIM slot N: subId=M`, by slot.
    pub slot_sub_ids: BTreeMap<u32, i64>,
    /// The SIM state of each slot, from `mSimState[N]=STATE`, by slot.
    pub sim_states: BTreeMap<u32, String>,
    /// `areAllSubscriptionsLoaded`, when it reads `true` or `false`.
    pub all_loaded: Option<bool>,
    /// `defaultDataSubId` at the start of a line.
    pub default_data_sub_id: Option<i64>,
    /// `defaultSmsSubId` at the start of a line.
    pub default_sms_sub_id: Option<i64>,
    /// The records under `Active subscriptions:`, in order.
    pub active: Vec<SubscriptionRecord>,
}

/// One `SubscriptionInfoInternal` record under `Active subscriptions:`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubscriptionRecord {
    /// `id`: the subscription id, which Android keeps for each ICCID.
    pub sub_id: i64,
    /// `simSlotIndex`: the logical slot.
    pub slot: u32,
    /// The leading digits of `iccId`. dumpsys masks the ICCID after a prefix, such as
    /// `890155500[****]`, and masks the number and the IMSI whole.
    pub iccid_prefix: Option<String>,
    /// `displayName`, such as the carrier's name.
    pub display_name: Option<String>,
    /// `carrierName`, which reflects service, such as `Emergency calls only`.
    pub carrier_name: Option<String>,
    /// `mcc`.
    pub mcc: Option<String>,
    /// `mnc`.
    pub mnc: Option<String>,
    /// `countryIso`, such as `us`.
    pub country_iso: Option<String>,
}

/// Reads the output of `dumpsys isub`. Lines it doesn't recognize are skipped, so any text,
/// even an error, gives a result: one with nothing in it.
///
/// A slot named twice keeps its last sub id or state. A record under `Active subscriptions:`
/// without an integer `id` and a slot of zero or more is skipped.
#[must_use]
pub fn parse_isub(output: &str) -> Isub {
    let mut isub = Isub::default();
    let mut section = "";
    for line in text::lines(output) {
        if is_section_header(line) {
            section = text::trim(line);
        }
        if let Some(mapping) = slot_mapping(line) {
            isub.slot_sub_ids.insert(mapping.slot, mapping.sub_id);
        }
        if let Some(state) = sim_state(text::trim(line)) {
            isub.sim_states.insert(state.slot, state.state.to_owned());
        }
        if section == "Active subscriptions:"
            && line.contains("simSlotIndex=")
            && let Some(record) = subscription_record(line)
        {
            isub.active.push(record);
        }
    }
    isub.active_modem_count = text::number_after(
        output,
        "Active modem count=",
        Sign::Unsigned,
        Start::Anywhere,
    )
    .and_then(text::parse_int);
    isub.all_loaded = all_loaded(output);
    isub.default_data_sub_id = line_start_number(output, "defaultDataSubId=");
    isub.default_sms_sub_id = line_start_number(output, "defaultSmsSubId=");
    isub
}

/// Whether the line names a section, as `/^\S.*:\s*$/` matches it: it starts with a character
/// other than white space and ends with a colon after that, then only white space.
fn is_section_header(line: &str) -> bool {
    let Some(first) = line.chars().next() else {
        return false;
    };
    if text::is_space(first) {
        return false;
    }
    let Some(body) = text::trim_end(line).strip_suffix(':') else {
        return false;
    };
    body.len() >= first.len_utf8() && !body.contains(text::is_line_terminator)
}

/// A `Logical SIM slot N: subId=M` mapping.
struct SlotMapping {
    slot: u32,
    sub_id: i64,
}

/// The first `Logical SIM slot (\d+): subId=(-?\d+)` in the line.
fn slot_mapping(line: &str) -> Option<SlotMapping> {
    const PREFIX: &str = "Logical SIM slot ";
    text::occurrences(line, PREFIX).find_map(|index| {
        let after = &line[index + PREFIX.len()..];
        let slot = text::leading_digits(after);
        let rest = after[slot.len()..].strip_prefix(": subId=")?;
        let sub_id = text::leading_number(rest, Sign::Signed)?;
        Some(SlotMapping {
            slot: slot.parse().ok()?,
            sub_id: text::parse_int(sub_id)?,
        })
    })
}

/// A `mSimState[N]=STATE` line, trimmed.
struct SimState<'line> {
    slot: u32,
    state: &'line str,
}

/// `/^mSimState\[(\d+)\]=(\S+)/` on a trimmed line.
fn sim_state(line: &str) -> Option<SimState<'_>> {
    let after = line.strip_prefix("mSimState[")?;
    let slot = text::leading_digits(after);
    let state = text::leading_non_space(after[slot.len()..].strip_prefix("]=")?);
    if slot.is_empty() || state.is_empty() {
        return None;
    }
    Some(SimState {
        slot: slot.parse().ok()?,
        state,
    })
}

/// `/areAllSubscriptionsLoaded=(\w+)/`, read as `true` or `false`.
fn all_loaded(output: &str) -> Option<bool> {
    const PREFIX: &str = "areAllSubscriptionsLoaded=";
    let value = text::occurrences(output, PREFIX).find_map(|index| {
        let after = &output[index + PREFIX.len()..];
        let end = after.find(|c| !text::is_word(c)).unwrap_or(after.len());
        (end > 0).then(|| &after[..end])
    })?;
    match value {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// The number in the first `/^<prefix>(-?\d+)/m` match: the prefix at the start of a line, where
/// any line terminator, a lone carriage return included, starts a line.
fn line_start_number(output: &str, prefix: &str) -> Option<i64> {
    output
        .split(text::is_line_terminator)
        .filter_map(|line| line.strip_prefix(prefix))
        .find_map(|rest| text::leading_number(rest, Sign::Signed))
        .and_then(text::parse_int)
}

/// One `SubscriptionInfoInternal: id=1 iccId=... carrierName=Emergency calls only ...` record.
/// Values can hold spaces, so each value runs to the next ` key=`; a key seen twice keeps its
/// first value.
fn subscription_record(line: &str) -> Option<SubscriptionRecord> {
    let body = &line[line.find("SubscriptionInfoInternal:")?..];
    let keys = record_keys(body);
    let mut fields: HashMap<&str, &str> = HashMap::new();
    for (index, key) in keys.iter().enumerate() {
        let end = keys.get(index + 1).map_or(body.len(), |next| next.start);
        fields
            .entry(key.name)
            .or_insert_with(|| text::trim(&body[key.value_start..end]));
    }
    let int = |key: &str| {
        fields
            .get(key)
            .and_then(|value| text::parse_int(text::trim(value)))
    };
    let non_empty = |key: &str| {
        fields
            .get(key)
            .filter(|value| !value.is_empty())
            .map(|value| (*value).to_owned())
    };
    let sub_id = int("id")?;
    let slot = u32::try_from(int("simSlotIndex")?).ok()?;
    let iccid_prefix = text::leading_digits(fields.get("iccId").copied().unwrap_or(""));
    Some(SubscriptionRecord {
        sub_id,
        slot,
        iccid_prefix: (!iccid_prefix.is_empty()).then(|| iccid_prefix.to_owned()),
        display_name: non_empty("displayName"),
        carrier_name: non_empty("carrierName"),
        mcc: non_empty("mcc"),
        mnc: non_empty("mnc"),
        country_iso: non_empty("countryIso"),
    })
}

/// Where a ` key=` starts in a record.
struct RecordKey<'body> {
    /// The index of the white space before the key, or of the key at the start of the body.
    start: usize,
    name: &'body str,
    /// The index just after the `=`.
    value_start: usize,
}

/// The matches of `/(?:^|\s)([a-zA-Z][a-zA-Z0-9]*)=/g` in the body, in order and without
/// overlap.
fn record_keys(body: &str) -> Vec<RecordKey<'_>> {
    let mut keys = Vec::new();
    let mut position = 0;
    while position < body.len() {
        let found = if position == 0 {
            key_at(body, 0, 0)
        } else {
            None
        }
        .or_else(|| {
            let c = body[position..].chars().next()?;
            if text::is_space(c) {
                key_at(body, position, position + c.len_utf8())
            } else {
                None
            }
        });
        match found {
            Some(key) => {
                position = key.value_start;
                keys.push(key);
            }
            None => {
                position += body[position..].chars().next().map_or(1, char::len_utf8);
            }
        }
    }
    keys
}

/// A key `[a-zA-Z][a-zA-Z0-9]*=` at `name_start`, for a match that starts at `start`.
fn key_at(body: &str, start: usize, name_start: usize) -> Option<RecordKey<'_>> {
    let rest = &body[name_start..];
    if !rest.starts_with(|c: char| c.is_ascii_alphabetic()) {
        return None;
    }
    let length = rest
        .find(|c: char| !c.is_ascii_alphanumeric())
        .unwrap_or(rest.len());
    rest[length..].starts_with('=').then(|| RecordKey {
        start,
        name: &rest[..length],
        value_start: name_start + length + 1,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn section_headers() {
        assert!(is_section_header("Active subscriptions:"));
        assert!(is_section_header("a:  \r"));
        assert!(is_section_header("::"));
        assert!(!is_section_header(":"));
        assert!(!is_section_header("  Indented:"));
        assert!(!is_section_header("Embedded subscriptions: []"));
        assert!(!is_section_header(""));
    }

    #[test]
    fn records_need_an_id_and_a_slot() {
        let header = "Active subscriptions:\n";
        let parse = |record: &str| parse_isub(&format!("{header}{record}")).active;
        assert_eq!(
            parse("  [SubscriptionInfoInternal: id=x simSlotIndex=0]"),
            []
        );
        assert_eq!(
            parse("  [SubscriptionInfoInternal: id=3 simSlotIndex=-1]"),
            []
        );
        assert_eq!(parse("  simSlotIndex=0 without the record name"), []);
        let records = parse("  [SubscriptionInfoInternal: id=3 iccId= simSlotIndex=1 id=9 mcc=");
        assert_eq!(
            records,
            [SubscriptionRecord {
                sub_id: 3,
                slot: 1,
                iccid_prefix: None,
                display_name: None,
                carrier_name: None,
                mcc: None,
                mnc: None,
                country_iso: None,
            }]
        );
    }

    #[test]
    fn records_outside_active_subscriptions_are_ignored() {
        let isub =
            parse_isub("All subscriptions:\n  [SubscriptionInfoInternal: id=1 simSlotIndex=0]\n");
        assert_eq!(isub.active, []);
    }

    #[test]
    fn keys_start_after_white_space_only() {
        let names: Vec<&str> = record_keys("a=1 b=x=y c= d=\te=5")
            .iter()
            .map(|key| key.name)
            .collect();
        assert_eq!(names, ["a", "b", "c", "d", "e"]);
    }

    #[test]
    fn whole_text_values() {
        let isub = parse_isub(
            "Active modem count=\nActive modem count=3\nareAllSubscriptionsLoaded=maybe\n \
             defaultDataSubId=4\ndefaultDataSubId=-\ndefaultDataSubId=-1\rdefaultSmsSubId=2",
        );
        assert_eq!(isub.active_modem_count, Some(3));
        assert_eq!(isub.all_loaded, None);
        assert_eq!(isub.default_data_sub_id, Some(-1));
        assert_eq!(isub.default_sms_sub_id, Some(2));
    }

    #[test]
    fn slot_lines() {
        let isub = parse_isub(
            "  Logical SIM slot 0: subId=1\nLogical SIM slot 1: subId=-1\n  mSimState[1]=ABSENT x\n\
             mSimState[0]=\nLogical SIM slot 0: subId=5\n",
        );
        assert_eq!(isub.slot_sub_ids, BTreeMap::from([(0, 5), (1, -1)]));
        assert_eq!(isub.sim_states, BTreeMap::from([(1, "ABSENT".to_owned())]));
    }
}
