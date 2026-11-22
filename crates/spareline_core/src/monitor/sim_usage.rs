//! The data watch: mobile bytes an app could have used over the internet, per SIM.

use std::cmp::Reverse;

use serde::Serialize;

use crate::monitor::{NetEntry, NetSection};

/// The phone process (Android's `PHONE_UID`). `MmsService`, which downloads and sends MMS for
/// Messages, and the IMS and video-call stacks run in it, so its bytes don't count.
pub const PHONE_UID: i64 = 1001;

/// The data watch's trailing window: 24 hours, in seconds.
pub const DATA_WINDOW_SECONDS: i64 = 86_400;

/// One SIM's mobile internet bytes over the window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SimUsage {
    /// The subscription.
    pub sub_id: i64,
    /// Bytes received.
    pub rx: u64,
    /// Bytes sent.
    pub tx: u64,
    /// The apps that used it, most first.
    pub uids: Vec<UidBytes>,
}

/// One app's bytes on a SIM, serialized as `{"uid":..,"bytes":..}`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub struct UidBytes {
    /// The app's uid.
    pub uid: i64,
    /// Bytes received and sent.
    pub bytes: u64,
}

/// A SIM's usage as a check report serves it: the API's `SimUsage`, with the SIM's slot and
/// whether it went over the threshold.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SimDataUsage {
    /// The subscription.
    pub sub_id: i64,
    /// Bytes received.
    pub rx: u64,
    /// Bytes sent.
    pub tx: u64,
    /// The apps that used it, most first.
    pub uids: Vec<UidBytes>,
    /// The slot the SIM is in, when an active subscription names it.
    pub slot: Option<u32>,
    /// Whether `rx + tx` is above the data threshold.
    pub over: bool,
}

impl SimUsage {
    /// The usage as a check report serves it, with the SIM's slot; `over` when `rx + tx` is
    /// above `threshold_bytes`.
    #[must_use]
    pub fn with_threshold(self, slot: Option<u32>, threshold_bytes: u64) -> SimDataUsage {
        SimDataUsage {
            sub_id: self.sub_id,
            slot,
            rx: self.rx,
            tx: self.tx,
            over: self.rx.saturating_add(self.tx) > threshold_bytes,
            uids: self.uids,
        }
    }
}

/// Mobile bytes an app could have used over the internet, per SIM, over the `window_seconds`
/// before `now_seconds` by the phone's clock (normally [`DATA_WINDOW_SECONDS`]), sorted by sub id.
///
/// It counts the per-uid entries on metered mobile networks with a sub id and tag `0x0` (tagged
/// rows are subsets of the untagged ones), leaving out [`PHONE_UID`]: the MMS APN is metered by
/// default and MMS must keep working with data off, and the B30 lists a metered video-calling
/// interface with data off. IMS itself runs on an unmetered APN. A bucket that overlaps the
/// window counts in full, which errs toward alerting. A SIM with no bytes isn't listed.
#[must_use]
pub fn internet_usage(
    entries: &[NetEntry],
    now_seconds: i64,
    window_seconds: i64,
) -> Vec<SimUsage> {
    let since = now_seconds.saturating_sub(window_seconds);
    let mut by_sub: Vec<SimUsage> = Vec::new();
    for entry in entries {
        let Some(sub_id) = entry.sub_id else {
            continue;
        };
        if entry.section != NetSection::Uid
            || !entry.mobile
            || !entry.metered
            || entry.tag != "0x0"
            || entry.uid == PHONE_UID
        {
            continue;
        }
        let mut rx: u64 = 0;
        let mut tx: u64 = 0;
        for bucket in &entry.buckets {
            if bucket.start.saturating_add(entry.bucket_seconds) > since {
                rx = rx.saturating_add(bucket.rx);
                tx = tx.saturating_add(bucket.tx);
            }
        }
        let bytes = rx.saturating_add(tx);
        if bytes == 0 {
            continue;
        }
        let index = if let Some(index) = by_sub.iter().position(|usage| usage.sub_id == sub_id) {
            index
        } else {
            by_sub.push(SimUsage {
                sub_id,
                rx: 0,
                tx: 0,
                uids: Vec::new(),
            });
            by_sub.len() - 1
        };
        let usage = &mut by_sub[index];
        usage.rx = usage.rx.saturating_add(rx);
        usage.tx = usage.tx.saturating_add(tx);
        match usage.uids.iter_mut().find(|item| item.uid == entry.uid) {
            Some(item) => item.bytes = item.bytes.saturating_add(bytes),
            None => usage.uids.push(UidBytes {
                uid: entry.uid,
                bytes,
            }),
        }
    }
    for usage in &mut by_sub {
        usage.uids.sort_by_key(|item| Reverse(item.bytes));
    }
    by_sub.sort_by_key(|usage| usage.sub_id);
    by_sub
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::NetBucket;

    fn entry(uid: i64, sub_id: Option<i64>, buckets: &[NetBucket]) -> NetEntry {
        NetEntry {
            section: NetSection::Uid,
            mobile: true,
            metered: true,
            sub_id,
            uid,
            tag: "0x0".to_owned(),
            bucket_seconds: 3600,
            buckets: buckets.to_vec(),
        }
    }

    fn bucket(start: i64, rx: u64, tx: u64) -> NetBucket {
        NetBucket { start, rx, tx }
    }

    #[test]
    fn sums_per_sim_with_the_apps_most_first() {
        let entries = [
            entry(10_190, Some(2), &[bucket(1000, 5, 5)]),
            entry(10_101, Some(1), &[bucket(1000, 100, 1)]),
            entry(10_190, Some(1), &[bucket(1000, 200, 0)]),
            entry(10_101, Some(1), &[bucket(1000, 0, 50)]),
        ];
        let usage = internet_usage(&entries, 2000, DATA_WINDOW_SECONDS);
        assert_eq!(
            usage,
            [
                SimUsage {
                    sub_id: 1,
                    rx: 300,
                    tx: 51,
                    uids: vec![
                        UidBytes {
                            uid: 10_190,
                            bytes: 200,
                        },
                        UidBytes {
                            uid: 10_101,
                            bytes: 151,
                        },
                    ],
                },
                SimUsage {
                    sub_id: 2,
                    rx: 5,
                    tx: 5,
                    uids: vec![UidBytes {
                        uid: 10_190,
                        bytes: 10,
                    }],
                },
            ]
        );
    }

    #[test]
    fn what_does_not_count() {
        let mut wifi = entry(1, Some(1), &[bucket(0, 9, 9)]);
        wifi.mobile = false;
        let mut unmetered = entry(1, Some(1), &[bucket(0, 9, 9)]);
        unmetered.metered = false;
        let mut tagged = entry(1, Some(1), &[bucket(0, 9, 9)]);
        tagged.tag = "0xffffff01".to_owned();
        let mut xt = entry(1, Some(1), &[bucket(0, 9, 9)]);
        xt.section = NetSection::Xt;
        let entries = [
            wifi,
            unmetered,
            tagged,
            xt,
            entry(PHONE_UID, Some(1), &[bucket(0, 9, 9)]),
            entry(1, None, &[bucket(0, 9, 9)]),
            entry(1, Some(1), &[bucket(0, 0, 0)]),
        ];
        assert_eq!(internet_usage(&entries, 10, DATA_WINDOW_SECONDS), []);
    }

    #[test]
    fn a_bucket_counts_while_it_overlaps_the_window() {
        let entries = [entry(1, Some(1), &[bucket(1000, 7, 0)])];
        assert_eq!(internet_usage(&entries, 4600 + 10, 5), []);
        assert_eq!(internet_usage(&entries, 4600 - 10 + 600, 600)[0].rx, 7);
        assert_eq!(internet_usage(&entries, 4600 + 4, 5)[0].rx, 7);
        assert_eq!(internet_usage(&entries, 4600 + 5, 5), []);
    }

    #[test]
    fn serializes_as_the_api_sim_usage() {
        let usage = SimUsage {
            sub_id: 1,
            rx: 600_000,
            tx: 50_000,
            uids: vec![UidBytes {
                uid: 10_101,
                bytes: 650_000,
            }],
        };
        let served = usage.clone().with_threshold(Some(0), 1_000_000);
        assert_eq!(
            serde_json::to_string(&served).expect("serializes"),
            r#"{"subId":1,"rx":600000,"tx":50000,"uids":[{"uid":10101,"bytes":650000}],"slot":0,"over":false}"#
        );
        assert!(
            usage.clone().with_threshold(None, 649_999).over,
            "over the threshold"
        );
        assert!(
            !usage.with_threshold(None, 650_000).over,
            "at the threshold is not over"
        );
    }
}
