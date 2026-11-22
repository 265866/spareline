//! What an identity probe found on a phone, as inventory records it.

/// The device facts and SIMs an identity probe read. The server maps the Android parsers'
/// output to this.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IdentityReading {
    /// `ro.product.manufacturer`.
    pub manufacturer: Option<String>,
    /// `ro.product.model`.
    pub model: Option<String>,
    /// `ro.build.version.release`.
    pub android_version: Option<String>,
    /// `ro.build.version.sdk`.
    pub sdk: Option<i64>,
    /// `ro.build.fingerprint`.
    pub fingerprint: Option<String>,
    /// Slot 0's IMEI.
    pub imei1: Option<String>,
    /// Slot 1's IMEI.
    pub imei2: Option<String>,
    /// Every slot the probe read.
    pub slots: Vec<SlotReading>,
}

/// One SIM slot as the probe read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlotReading {
    /// The slot, from 0.
    pub slot: i64,
    /// What is in it.
    pub contents: SlotContents,
}

/// What a slot holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SlotContents {
    /// A loaded SIM with a subscription, and what the probe read of it, if anything.
    Present(Option<SimReading>),
    /// No card: an open placement in the slot is closed.
    Empty,
    /// Anything else, such as booting, PIN locked or a card error: the slot is left as it was,
    /// so a SIM that is still loading isn't recorded as removed.
    Unknown,
}

/// A SIM as the probe read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SimReading {
    /// Android's subscription id.
    pub sub_id: i64,
    /// The ICCID; without one the slot is skipped.
    pub iccid: Option<String>,
    /// The carrier's display name.
    pub carrier: Option<String>,
    /// The number the SIM reports, normalized to E.164.
    pub number: Option<String>,
}
