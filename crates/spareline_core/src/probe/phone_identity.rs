//! The identity probe: what changes only when the phone or its SIMs change.

use std::collections::BTreeMap;

use crate::android::{self, Isub, ParcelString};
use crate::identifiers::{self, SubInfoCodes, SubInfoLookup};
use crate::text;

/// The properties the identity script reads with `getprop`, in order.
const PROPS: [&str; 8] = [
    "ro.product.manufacturer",
    "ro.product.model",
    "ro.build.version.release",
    "ro.build.version.sdk",
    "ro.build.fingerprint",
    "ro.build.version.security_patch",
    "persist.radio.multisim.config",
    "gsm.sim.state",
];

/// The most slots an identity holds. Phones have one to three, and spareline-old took any modem
/// count as given; a count above this one, which only garbled output could hold, is read as this
/// many rather than allocating a slot, and sending a getter call, for each.
const MAX_SLOTS: usize = 256;

/// The first identity script: the build properties and `dumpsys isub`, in sections `props` and
/// `isub`. It is
///
/// ```text
/// printf '\n@@sl:props\n'; for p in ro.product.manufacturer ... gsm.sim.state; do echo "$p=$(getprop $p)"; done; printf '\n@@sl:isub\n'; dumpsys isub
/// ```
#[must_use]
pub fn identity_command() -> String {
    [
        android::section_marker("props"),
        format!(
            "for p in {}; do echo \"$p=$(getprop $p)\"; done",
            PROPS.join(" ")
        ),
        android::section_marker("isub"),
        "dumpsys isub".to_owned(),
    ]
    .join("; ")
}

/// A phone's identity, as of its last identity probe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhoneIdentity {
    /// `ro.product.manufacturer`.
    pub manufacturer: Option<String>,
    /// `ro.product.model`.
    pub model: Option<String>,
    /// `ro.build.version.release`, such as `14`.
    pub android_version: Option<String>,
    /// `ro.build.version.sdk`, such as 34.
    pub sdk: Option<u64>,
    /// `ro.build.fingerprint`.
    pub fingerprint: Option<String>,
    /// `ro.build.version.security_patch`, such as `2024-09-05`.
    pub security_patch: Option<String>,
    /// The IMEI of each slot, by slot; `None` when unreadable.
    pub imeis: Vec<Option<String>>,
    /// Each slot's SIM state, by slot.
    pub slots: Vec<SlotState>,
    /// The SIMs in slots that are [`SlotPresence::Present`], in `dumpsys isub`'s order.
    pub sims: Vec<SimIdentity>,
    /// `defaultDataSubId`.
    pub default_data_sub_id: Option<i64>,
    /// `defaultSmsSubId`.
    pub default_sms_sub_id: Option<i64>,
    /// Whether the subscriber getters' codes were checked on this exact build.
    pub sub_info_verified: bool,
    /// Why a field is missing, for the API and the log.
    pub warnings: Vec<String>,
}

/// One slot's SIM state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlotState {
    /// The logical slot.
    pub slot: u32,
    /// What inventory makes of the state.
    pub state: SlotPresence,
    /// Android's SIM state, such as `LOADED` or `ABSENT`: `dumpsys isub`'s, else `gsm.sim.state`'s.
    pub sim_state: Option<String>,
}

/// What inventory makes of a slot's SIM state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SlotPresence {
    /// A loaded SIM with an active subscription, once every subscription has loaded.
    Present,
    /// No card: `ABSENT`.
    Empty,
    /// Anything else, such as booting, PIN locked or a card error, which inventory leaves alone.
    Unknown,
}

impl SlotPresence {
    /// The state's name as spareline-old spelled it: `present`, `empty` or `unknown`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Present => "present",
            Self::Empty => "empty",
            Self::Unknown => "unknown",
        }
    }
}

/// A SIM in a present slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SimIdentity {
    /// The logical slot.
    pub slot: u32,
    /// The subscription id.
    pub sub_id: i64,
    /// The full ICCID, when the subscriber getter returned one of the right shape.
    pub iccid: Option<String>,
    /// The leading digits `dumpsys isub` leaves unmasked.
    pub iccid_prefix: Option<String>,
    /// The subscription's display name.
    pub carrier: Option<String>,
    /// The SIM's country, such as `us`.
    pub country_iso: Option<String>,
    /// The mobile country code.
    pub mcc: Option<String>,
    /// The mobile network code.
    pub mnc: Option<String>,
    /// The SIM's number in E.164, when it reports one that can be normalized.
    pub number: Option<String>,
}

/// An identity read from the first script, waiting for the subscriber getters.
///
/// The getters need the SDK, for their binder codes, and the sub ids the first script reports,
/// so they run as a second script: run [`IdentityProbe::subinfo_command`] when it is `Some` and
/// pass its output to [`IdentityProbe::apply_subinfo`], else take
/// [`IdentityProbe::into_identity`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdentityProbe {
    identity: PhoneIdentity,
    codes: Option<SubInfoCodes>,
}

/// Reads the output of [`identity_command`]. A section that is missing reads as empty, so the
/// result always exists; its warnings say what couldn't be read.
#[must_use]
pub fn parse_identity(stdout: &str) -> IdentityProbe {
    let sections = android::split_sections(stdout);
    let props = android::parse_key_values(sections.get("props").unwrap_or(""));
    let prop = |key: &str| props.get(key).map(String::as_str);
    let isub = android::parse_isub(sections.get("isub").unwrap_or(""));
    let sdk = prop("ro.build.version.sdk").and_then(|value| {
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        value.parse().ok()
    });
    let fingerprint = non_empty(prop("ro.build.fingerprint"));
    let prop_states = android::parse_sim_states(prop("gsm.sim.state").unwrap_or(""));
    let slot_count = slot_count(&props, &isub, prop_states.len());
    let slots = slot_states(slot_count, &isub, &prop_states);
    let sims: Vec<SimIdentity> = isub
        .active
        .iter()
        .filter(|sub| {
            slots
                .iter()
                .any(|slot| slot.slot == sub.slot && slot.state == SlotPresence::Present)
        })
        .map(|sub| SimIdentity {
            slot: sub.slot,
            sub_id: sub.sub_id,
            iccid: None,
            iccid_prefix: sub.iccid_prefix.clone(),
            carrier: sub.display_name.clone(),
            country_iso: sub.country_iso.clone(),
            mcc: sub.mcc.clone(),
            mnc: sub.mnc.clone(),
            number: None,
        })
        .collect();
    let mut warnings = Vec::new();
    let (codes, verified) = match identifiers::sub_info_codes(sdk, fingerprint.as_deref()) {
        SubInfoLookup::Codes { codes, verified } => {
            if !verified && let Some(sdk) = sdk {
                warnings.push(format!(
                    "iphonesubinfo codes for SDK {sdk} are unverified on this build"
                ));
            }
            (Some(codes), verified)
        }
        SubInfoLookup::Missing { reason } => {
            warnings.push(reason);
            (None, false)
        }
    };
    IdentityProbe {
        identity: PhoneIdentity {
            manufacturer: non_empty(prop("ro.product.manufacturer")),
            model: non_empty(prop("ro.product.model")),
            android_version: non_empty(prop("ro.build.version.release")),
            sdk,
            fingerprint,
            security_patch: non_empty(prop("ro.build.version.security_patch")),
            imeis: vec![None; slots.len()],
            slots,
            sims,
            default_data_sub_id: isub.default_data_sub_id,
            default_sms_sub_id: isub.default_sms_sub_id,
            sub_info_verified: verified,
            warnings,
        },
        codes,
    }
}

impl IdentityProbe {
    /// The identity as the first script gave it: no IMEIs, ICCIDs or numbers yet.
    #[must_use]
    pub fn identity(&self) -> &PhoneIdentity {
        &self.identity
    }

    /// The second script, which calls the subscriber getters: the IMEI of every slot, then the
    /// ICCID and number of every SIM, in sections `imei:<slot>`, `iccid:<subId>` and
    /// `number:<subId>`. `None` when the table has no codes for the phone's SDK, and then the
    /// identity has no identifiers to read.
    #[must_use]
    pub fn subinfo_command(&self) -> Option<String> {
        let codes = self.codes?;
        let mut parts = Vec::new();
        for slot in &self.identity.slots {
            parts.push(android::section_marker(&format!("imei:{}", slot.slot)));
            parts.push(identifiers::sub_info_call(
                codes.imei_for_slot,
                i64::from(slot.slot),
            ));
        }
        for sim in &self.identity.sims {
            parts.push(android::section_marker(&format!("iccid:{}", sim.sub_id)));
            parts.push(identifiers::sub_info_call(codes.iccid_for_sub, sim.sub_id));
            parts.push(android::section_marker(&format!("number:{}", sim.sub_id)));
            parts.push(identifiers::sub_info_call(codes.number_for_sub, sim.sub_id));
        }
        Some(parts.join("; "))
    }

    /// The identity with the identifiers read from the output of
    /// [`IdentityProbe::subinfo_command`]. A value that is missing, unreadable, or of the wrong
    /// shape stays `None` with a warning; a null or empty one stays `None` without one. Without
    /// codes there is nothing to read, and this is [`IdentityProbe::into_identity`].
    #[must_use]
    pub fn apply_subinfo(self, stdout: &str) -> PhoneIdentity {
        let mut identity = self.identity;
        if self.codes.is_none() {
            return identity;
        }
        let sections = android::split_sections(stdout);
        let mut warnings = Vec::new();
        for (imei, slot) in identity.imeis.iter_mut().zip(&identity.slots) {
            *imei = identifier(
                sections.get(&format!("imei:{}", slot.slot)),
                identifiers::is_imei,
                &format!("IMEI of slot {}", slot.slot),
                &mut warnings,
            );
        }
        for sim in &mut identity.sims {
            sim.iccid = identifier(
                sections.get(&format!("iccid:{}", sim.sub_id)),
                identifiers::is_iccid,
                &format!("ICCID of sub {}", sim.sub_id),
                &mut warnings,
            );
            let raw = identifier(
                sections.get(&format!("number:{}", sim.sub_id)),
                |value| !text::trim(value).is_empty(),
                &format!("number of sub {}", sim.sub_id),
                &mut warnings,
            );
            sim.number = raw
                .as_deref()
                .and_then(|raw| identifiers::to_e164(raw, sim.country_iso.as_deref()));
            if raw.is_some() && sim.number.is_none() {
                warnings.push(format!(
                    "number of sub {} is not a recognizable phone number",
                    sim.sub_id
                ));
            }
        }
        identity.warnings.append(&mut warnings);
        identity
    }

    /// The identity without the subscriber getters, for a phone whose SDK has no codes.
    #[must_use]
    pub fn into_identity(self) -> PhoneIdentity {
        self.identity
    }
}

/// One identifier from a getter's section, checked by `valid`.
fn identifier(
    output: Option<&str>,
    valid: impl Fn(&str) -> bool,
    what: &str,
    warnings: &mut Vec<String>,
) -> Option<String> {
    let Some(output) = output else {
        warnings.push(format!("{what}: no output"));
        return None;
    };
    match android::parse_parcel_string(output) {
        ParcelString::Invalid { reason } => {
            warnings.push(format!("{what}: {reason}"));
            None
        }
        ParcelString::Exception { code, message } => {
            let message = message.unwrap_or_default();
            warnings
                .push(text::trim(&format!("{what}: binder exception {code} {message}")).to_owned());
            None
        }
        ParcelString::Value(None) => None,
        ParcelString::Value(Some(value)) if value.is_empty() => None,
        ParcelString::Value(Some(value)) => {
            if valid(&value) {
                Some(value)
            } else {
                warnings.push(format!("{what}: unexpected value shape, ignored"));
                None
            }
        }
    }
}

/// How many slots the phone has: the modem count `dumpsys isub` reports, else what the
/// multi-SIM configuration implies, else the number of states in `gsm.sim.state`, at least one.
fn slot_count(props: &BTreeMap<String, String>, isub: &Isub, prop_states: usize) -> usize {
    let count = match isub.active_modem_count {
        Some(count) if count > 0 => usize::try_from(count).unwrap_or(MAX_SLOTS),
        _ => match props
            .get("persist.radio.multisim.config")
            .map(String::as_str)
        {
            Some("dsds" | "dsda") => 2,
            Some("tsts") => 3,
            _ => prop_states.max(1),
        },
    };
    count.min(MAX_SLOTS)
}

fn slot_states(count: usize, isub: &Isub, prop_states: &[String]) -> Vec<SlotState> {
    (0..count)
        .filter_map(|index| u32::try_from(index).ok())
        .map(|slot| {
            let sim_state = isub
                .sim_states
                .get(&slot)
                .or_else(|| prop_states.get(usize::try_from(slot).ok()?))
                .cloned();
            let has_sub = isub.active.iter().any(|sub| sub.slot == slot);
            let state = match sim_state.as_deref() {
                Some("ABSENT") => SlotPresence::Empty,
                Some("LOADED") if has_sub && isub.all_loaded != Some(false) => {
                    SlotPresence::Present
                }
                _ => SlotPresence::Unknown,
            };
            SlotState {
                slot,
                state,
                sim_state,
            }
        })
        .collect()
}

/// The value trimmed, unless it is missing or blank.
fn non_empty(value: Option<&str>) -> Option<String> {
    value
        .map(text::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity_output(props: &str, isub: &str) -> String {
        format!("\n@@sl:props\n{props}\n@@sl:isub\n{isub}\n")
    }

    #[test]
    fn empty_output_has_one_unknown_slot_and_a_warning() {
        let probe = parse_identity("");
        assert_eq!(probe.subinfo_command(), None);
        let identity = probe.into_identity();
        assert_eq!(
            identity.slots,
            [SlotState {
                slot: 0,
                state: SlotPresence::Unknown,
                sim_state: None,
            }]
        );
        assert_eq!(identity.imeis, [None]);
        assert_eq!(
            identity.warnings,
            [
                "no iphonesubinfo codes for Android SDK unknown; add them to \
                 crates/spareline_core/src/identifiers.rs"
            ]
        );
    }

    #[test]
    fn slot_count_falls_back_to_the_multisim_config_then_the_sim_states() {
        let count = |props: &str, isub: &str| {
            parse_identity(&identity_output(props, isub))
                .identity()
                .slots
                .len()
        };
        assert_eq!(count("persist.radio.multisim.config=tsts", ""), 3);
        assert_eq!(count("persist.radio.multisim.config=dsda", ""), 2);
        assert_eq!(count("gsm.sim.state=A,B,C,D", ""), 4);
        assert_eq!(count("gsm.sim.state=A,B", "Active modem count=0"), 2);
        assert_eq!(count("", "Active modem count=1"), 1);
        assert_eq!(count("", "Active modem count=999999"), MAX_SLOTS);
    }

    #[test]
    fn a_loaded_slot_is_unknown_until_every_subscription_has_loaded() {
        let isub = "areAllSubscriptionsLoaded=false\nmSimState[0]=LOADED\nActive subscriptions:\n  \
                    [SubscriptionInfoInternal: id=1 simSlotIndex=0 mcc=310]";
        let identity = parse_identity(&identity_output("", isub)).into_identity();
        assert_eq!(identity.slots[0].state, SlotPresence::Unknown);
        assert_eq!(identity.sims, []);
    }

    #[test]
    fn a_loaded_slot_without_a_subscription_is_unknown() {
        let identity = parse_identity(&identity_output("gsm.sim.state=LOADED", "")).into_identity();
        assert_eq!(identity.slots[0].sim_state.as_deref(), Some("LOADED"));
        assert_eq!(identity.slots[0].state, SlotPresence::Unknown);
    }

    #[test]
    fn getter_failures_become_warnings() {
        let props = "ro.build.version.sdk=34";
        let isub = "Active modem count=1\nmSimState[0]=LOADED\nActive subscriptions:\n  \
                    [SubscriptionInfoInternal: id=7 simSlotIndex=0 countryIso=gb]";
        let probe = parse_identity(&identity_output(props, isub));
        let subinfo = "@@sl:imei:0\nResult: Parcel(\n0x00000000: ffffffff 00000000 '........')\n\
                       @@sl:iccid:7\nservice: Service iphonesubinfo does not exist\n\
                       @@sl:number:7\nResult: Parcel(00000000 00000002 00320031 '....1.2.')";
        let identity = probe.apply_subinfo(subinfo);
        assert_eq!(identity.imeis, [None]);
        assert_eq!(identity.sims[0].iccid, None);
        assert_eq!(identity.sims[0].number, None);
        assert_eq!(
            identity.warnings,
            [
                "iphonesubinfo codes for SDK 34 are unverified on this build",
                "IMEI of slot 0: binder exception -1",
                "ICCID of sub 7: service: Service iphonesubinfo does not exist",
                "number of sub 7 is not a recognizable phone number",
            ]
        );
    }

    #[test]
    fn missing_and_null_getter_output() {
        let props = "ro.build.version.sdk=34";
        let isub = "Active modem count=1\nmSimState[0]=LOADED\nActive subscriptions:\n  \
                    [SubscriptionInfoInternal: id=7 simSlotIndex=0 mcc=310]";
        let probe = parse_identity(&identity_output(props, isub));
        let subinfo = "@@sl:iccid:7\nResult: Parcel(00000000 ffffffff '........')\n\
                       @@sl:number:7\nResult: Parcel(00000000 00000001 00000020 '....')";
        let identity = probe.apply_subinfo(subinfo);
        assert_eq!(
            identity.warnings[1..],
            [
                "IMEI of slot 0: no output",
                "number of sub 7: unexpected value shape, ignored",
            ],
            "a null ICCID warns of nothing, and a blank number is the wrong shape"
        );
        assert_eq!(identity.sims[0].number, None);
    }

    #[test]
    fn without_codes_the_subinfo_output_is_ignored() {
        let probe = parse_identity(&identity_output("ro.build.version.sdk=36", ""));
        assert_eq!(
            probe.clone().apply_subinfo("anything"),
            probe.into_identity()
        );
    }

    #[test]
    fn presence_names() {
        assert_eq!(SlotPresence::Present.as_str(), "present");
        assert_eq!(SlotPresence::Empty.as_str(), "empty");
        assert_eq!(SlotPresence::Unknown.as_str(), "unknown");
    }
}
