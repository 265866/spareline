//! A phone as the API shows it: what the database knows, joined with what the phone manager
//! sees now.

use std::collections::BTreeSet;

use serde::{Serialize, Serializer};
use spareline_core::{Battery, RadioState, Timestamp};
use spareline_db::{NumberSummary, PhoneRecord, PhoneStatus};

use crate::devices::LivePhone;

/// The API's `Phone`, with its fields in the old schema's order.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PhoneView {
    id: i64,
    serial: String,
    site: String,
    manufacturer: Option<String>,
    model: Option<String>,
    android_version: Option<String>,
    imeis: Vec<String>,
    location: Option<String>,
    hub_port: Option<String>,
    cost_cents: Option<i64>,
    acquired_on: Option<String>,
    status: &'static str,
    adb_state: String,
    #[serde(serialize_with = "optional_time")]
    status_since: Option<Timestamp>,
    first_seen_at: String,
    last_online_at: Option<String>,
    battery: Option<Battery>,
    slots: Vec<SlotView>,
    #[serde(serialize_with = "optional_time")]
    health_at: Option<Timestamp>,
    #[serde(serialize_with = "optional_time")]
    identity_at: Option<Timestamp>,
    warnings: Vec<String>,
    probe_error: Option<String>,
}

/// One SIM slot of a phone: the union of the slots the live identity lists and the slots with
/// a SIM placed now.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct SlotView {
    slot: i64,
    sim_state: Option<String>,
    radio: Option<RadioState>,
    sim: Option<SlotSim>,
    number: Option<NumberSummary>,
}

/// The SIM in a slot: `{id, iccid, carrier, subId}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct SlotSim {
    id: i64,
    iccid: String,
    carrier: Option<String>,
    sub_id: i64,
}

impl PhoneView {
    /// Joins the stored phone with its live state. A phone this process hasn't seen shows its
    /// last stored state, and the live fields are null or empty.
    pub(crate) fn new(record: PhoneRecord, live: Option<&LivePhone>) -> Self {
        let health = live.and_then(|live| live.health.as_ref());
        let identity = live.and_then(|live| live.identity.as_ref());
        let mut slot_numbers: BTreeSet<i64> = BTreeSet::new();
        for slot in identity.map_or_default(|identity| identity.slots.as_slice()) {
            slot_numbers.insert(i64::from(slot.slot));
        }
        for placement in &record.placements {
            slot_numbers.insert(placement.slot);
        }
        let slots: Vec<SlotView> = slot_numbers
            .into_iter()
            .map(|slot| {
                let placement = record
                    .placements
                    .iter()
                    .find(|placement| placement.slot == slot);
                let health_state = health.and_then(|health| {
                    usize::try_from(slot)
                        .ok()
                        .and_then(|index| health.sim_states.get(index).cloned())
                });
                let identity_state = || {
                    identity.and_then(|identity| {
                        identity
                            .slots
                            .iter()
                            .find(|candidate| i64::from(candidate.slot) == slot)
                            .and_then(|candidate| candidate.sim_state.clone())
                    })
                };
                SlotView {
                    slot,
                    sim_state: health_state.or_else(identity_state),
                    radio: health.and_then(|health| radio_of(&health.radios, slot)),
                    sim: placement.map(|placement| SlotSim {
                        id: placement.sim.id,
                        iccid: placement.sim.iccid.clone(),
                        carrier: placement.sim.carrier.clone(),
                        sub_id: placement.sub_id,
                    }),
                    number: placement.and_then(|placement| placement.number.clone()),
                }
            })
            .collect();
        let status = match live {
            Some(live) => live.status.as_str(),
            None => PhoneStatus::of_state(&record.last_state).as_str(),
        };
        let adb_state = live.map_or_else(
            || record.last_state.clone(),
            |live| live.adb_state.as_str().to_owned(),
        );
        Self {
            id: record.id,
            serial: record.serial,
            site: record.site,
            manufacturer: record.manufacturer,
            model: record.model,
            android_version: record.android_version,
            imeis: record.imeis,
            location: record.location,
            hub_port: record.hub_port,
            cost_cents: record.cost_cents,
            acquired_on: record.acquired_on,
            status,
            adb_state,
            status_since: live.map(|live| live.since),
            first_seen_at: record.first_seen_at,
            last_online_at: record.last_online_at,
            battery: health.and_then(|health| health.battery.clone()),
            slots,
            health_at: live.and_then(|live| live.health_at),
            identity_at: live.and_then(|live| live.identity_at),
            warnings: identity.map_or_default(|identity| identity.warnings.clone()),
            probe_error: live.and_then(|live| live.probe_error.clone()),
        }
    }
}

/// The radio of `slot` among `radios`.
pub(crate) fn radio_of(radios: &[RadioState], slot: i64) -> Option<RadioState> {
    radios
        .iter()
        .find(|radio| i64::from(radio.slot) == slot)
        .cloned()
}

/// Writes an optional time in the 24-character form, or `null`.
#[expect(
    clippy::ref_option,
    reason = "serde's serialize_with hands the function a reference to the field itself"
)]
pub(crate) fn optional_time<S: Serializer>(
    time: &Option<Timestamp>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match time {
        Some(time) => serializer.collect_str(time),
        None => serializer.serialize_none(),
    }
}
