//! A phone as the database knows it.

use serde::Serialize;

use crate::field_patch::FieldPatch;
use crate::number_summary::NumberSummary;
use crate::sim_summary::SimSummary;

/// A phone's stored facts and the SIMs placed in it now. The API's phone view adds live state
/// the server owns: `status` (from live state, else [`PhoneStatus::of_state`] of
/// [`PhoneRecord::last_state`]), `adbState`, `statusSince`, `battery`, the slots' `simState`
/// and `radio`, `healthAt`, `identityAt`, `warnings` and `probeError`.
///
/// [`PhoneStatus::of_state`]: crate::PhoneStatus::of_state
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhoneRecord {
    /// The row id.
    pub id: i64,
    /// The adb serial.
    pub serial: String,
    /// The site the phone was first seen at.
    pub site: String,
    /// `ro.product.manufacturer`.
    pub manufacturer: Option<String>,
    /// `ro.product.model`.
    pub model: Option<String>,
    /// `ro.build.version.release`.
    pub android_version: Option<String>,
    /// `ro.build.version.sdk`, which the API doesn't show.
    pub sdk: Option<i64>,
    /// The IMEIs that are known, slot 0's first.
    pub imeis: Vec<String>,
    /// Where the phone is, typed by a person.
    pub location: Option<String>,
    /// Which hub port it is on, typed by a person.
    pub hub_port: Option<String>,
    /// What it cost, in cents.
    pub cost_cents: Option<i64>,
    /// When it was bought, `YYYY-MM-DD`.
    pub acquired_on: Option<String>,
    /// The last settled adb state, such as `device` or `gone`.
    pub last_state: String,
    /// When the phone was first seen.
    pub first_seen_at: String,
    /// When it was last online.
    pub last_online_at: Option<String>,
    /// The SIMs in it now, by slot.
    pub placements: Vec<PlacementRecord>,
}

/// A SIM placed in a phone now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlacementRecord {
    /// The slot, from 0.
    pub slot: i64,
    /// Android's subscription id for the SIM.
    pub sub_id: i64,
    /// When the SIM was first seen in this slot.
    pub since: String,
    /// The SIM.
    pub sim: SimSummary,
    /// The number its open binding names.
    pub number: Option<NumberSummary>,
}

/// The fields `PATCH /api/v1/phones/:serial` may change. Written to `inventory_edits` as the
/// request had them, in this order.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PhonePatch {
    /// Where the phone is, at most 64 characters.
    #[serde(skip_serializing_if = "FieldPatch::is_absent")]
    pub location: FieldPatch<String>,
    /// Which hub port it is on, at most 64 characters.
    #[serde(skip_serializing_if = "FieldPatch::is_absent")]
    pub hub_port: FieldPatch<String>,
    /// What it cost, in cents, at least 0.
    #[serde(skip_serializing_if = "FieldPatch::is_absent")]
    pub cost_cents: FieldPatch<i64>,
    /// When it was bought, `YYYY-MM-DD`.
    #[serde(skip_serializing_if = "FieldPatch::is_absent")]
    pub acquired_on: FieldPatch<String>,
}
