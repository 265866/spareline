//! A phone's health over five minutes, as `health_5m` stores it.

use std::collections::BTreeMap;

use spareline_core::Timestamp;

/// The five-minute bucket length, in milliseconds. Buckets start at multiples of it since the
/// Unix epoch.
pub const BUCKET_MILLIS: i64 = 5 * 60_000;

/// The samples of one phone in one five-minute bucket, summed as the monitor collects them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HealthBucket {
    /// The bucket's start, a multiple of [`BUCKET_MILLIS`].
    pub start: Timestamp,
    /// How many samples it holds.
    pub samples: i64,
    /// The sum of the battery levels read.
    pub battery_sum: i64,
    /// How many battery levels were read.
    pub battery_count: i64,
    /// The highest battery temperature read, in tenths of a degree Celsius.
    pub temp_max: Option<i64>,
    /// The radios' samples, by slot. Only slots whose SIM was `LOADED` count.
    pub slots: BTreeMap<i64, SlotHealth>,
}

impl HealthBucket {
    /// The start of the bucket `at` falls in.
    #[must_use]
    pub fn start_of(at: Timestamp) -> Timestamp {
        Timestamp::from_millis(at.millis().div_euclid(BUCKET_MILLIS) * BUCKET_MILLIS)
    }
}

/// One slot's radio samples in a bucket.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SlotHealth {
    /// The lowest signal level read, 0 to 4.
    pub min_level: i64,
    /// The sum of the signal strengths read, in dBm.
    pub dbm_sum: i64,
    /// How many signal strengths were read.
    pub dbm_count: i64,
    /// How many samples found voice in service.
    pub in_service: i64,
    /// How many samples it holds.
    pub samples: i64,
}

/// Mobile data in the trailing 24 hours at the last data watch, in bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MobileBytes {
    /// Received.
    pub rx: i64,
    /// Sent.
    pub tx: i64,
}
