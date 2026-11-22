//! The `health_5m` table: five-minute health aggregates, written from the monitor's in-memory
//! buckets so the SD card sees few writes, and never read by spareline itself.

use std::collections::BTreeMap;
use std::time::Duration;

use rusqlite::Connection;
use serde::Serialize;
use serde_json::Number;
use spareline_core::Timestamp;

use crate::database_error::DatabaseError;
use crate::health_bucket::{HealthBucket, MobileBytes};

/// How long buckets are kept: 90 days.
pub const HEALTH_RETENTION: Duration = Duration::from_hours(90 * 24);

/// The health aggregates.
#[derive(Debug)]
pub struct HealthStore<'db> {
    connection: &'db mut Connection,
}

impl<'db> HealthStore<'db> {
    pub(crate) fn new(connection: &'db mut Connection) -> Self {
        Self { connection }
    }
}

impl HealthStore<'_> {
    /// Writes the bucket of the phone with `serial`, replacing a row of the same start, as
    /// spareline-old's `HealthRecorder` did: the rounded mean battery level, the highest
    /// temperature, `signal_json` as `{"<slot>": {"minLevel", "dbm"}}` with the rounded mean
    /// dBm, `service_json` as `{"<slot>": <share in service, to 2 decimals>}`, and the mobile
    /// bytes. Returns whether it wrote: an unknown phone or an empty bucket writes nothing.
    ///
    /// # Errors
    ///
    /// Fails when SQLite does.
    pub fn write(
        &mut self,
        serial: &str,
        bucket: &HealthBucket,
        mobile: Option<MobileBytes>,
    ) -> Result<bool, DatabaseError> {
        use rusqlite::OptionalExtension as _;
        let phone_id: Option<i64> = self
            .connection
            .query_row("SELECT id FROM phones WHERE serial = ?", [serial], |row| {
                row.get(0)
            })
            .optional()?;
        let Some(phone_id) = phone_id.filter(|_| bucket.samples != 0) else {
            return Ok(false);
        };
        let mut signal: BTreeMap<i64, SignalJson> = BTreeMap::new();
        let mut service: BTreeMap<i64, Number> = BTreeMap::new();
        for (slot, health) in &bucket.slots {
            signal.insert(
                *slot,
                SignalJson {
                    min_level: health.min_level,
                    dbm: (health.dbm_count > 0)
                        .then(|| js_round_ratio(health.dbm_sum, health.dbm_count)),
                },
            );
            service.insert(*slot, service_share(health.in_service, health.samples));
        }
        let signal_json = serde_json::to_string(&signal)
            .map_err(|source| DatabaseError::json("signal_json".to_owned(), source))?;
        let service_json = serde_json::to_string(&service)
            .map_err(|source| DatabaseError::json("service_json".to_owned(), source))?;
        let battery = (bucket.battery_count > 0)
            .then(|| js_round_ratio(bucket.battery_sum, bucket.battery_count));
        self.connection.execute(
            "INSERT OR REPLACE INTO health_5m
                (phone_id, bucket, samples, battery_pct, battery_temp_dc, signal_json, service_json, mobile_rx, mobile_tx)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            rusqlite::params![
                phone_id,
                bucket.start.to_string(),
                bucket.samples,
                battery,
                bucket.temp_max,
                signal_json,
                service_json,
                mobile.map(|bytes| bytes.rx),
                mobile.map(|bytes| bytes.tx),
            ],
        )?;
        Ok(true)
    }

    /// Deletes the buckets that started more than [`HEALTH_RETENTION`] before `now`, and
    /// returns how many there were. spareline-old ran it at the first sample after start, then
    /// at most once a day.
    ///
    /// # Errors
    ///
    /// Fails when SQLite does.
    pub fn prune(&mut self, now: Timestamp) -> Result<usize, DatabaseError> {
        let deleted = self.connection.execute(
            "DELETE FROM health_5m WHERE bucket < ?",
            [now.minus(HEALTH_RETENTION).to_string()],
        )?;
        Ok(deleted)
    }
}

/// One slot of `signal_json`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SignalJson {
    min_level: i64,
    dbm: Option<i64>,
}

/// `sum / count` rounded as JavaScript's `Math.round` rounds: halves toward positive infinity,
/// so -110.5 is -110.
#[expect(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    reason = "spareline-old computed this in JavaScript doubles; sums of battery levels and dBm readings stay far below 2^53"
)]
fn js_round_ratio(sum: i64, count: i64) -> i64 {
    js_round(sum as f64 / count as f64) as i64
}

/// The share of `samples` in service, rounded to two decimals as spareline-old wrote it:
/// `Math.round(share * 100) / 100`, a whole number written without a fraction.
#[expect(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    reason = "spareline-old computed this in JavaScript doubles; sample counts stay far below 2^53"
)]
fn service_share(in_service: i64, samples: i64) -> Number {
    let hundredths = js_round(in_service as f64 / samples as f64 * 100.0);
    let share = hundredths / 100.0;
    if share.fract() == 0.0 {
        Number::from(share as i64)
    } else {
        Number::from_f64(share).unwrap_or_else(|| Number::from(0))
    }
}

/// JavaScript's `Math.round`: the nearest integer, halves toward positive infinity.
fn js_round(value: f64) -> f64 {
    let floor = value.floor();
    if value - floor >= 0.5 {
        floor + 1.0
    } else {
        floor
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounds_as_javascript_does() {
        assert_eq!(js_round(2.5), 3.0);
        assert_eq!(js_round(-2.5), -2.0);
        assert_eq!(js_round(-110.5), -110.0);
        assert_eq!(js_round(0.499_999_999_999_999_94), 0.0);
        assert_eq!(js_round_ratio(-221, 2), -110);
        assert_eq!(js_round_ratio(171, 2), 86);
    }

    #[test]
    fn writes_shares_as_javascript_prints_them() {
        let text = |in_service, samples| service_share(in_service, samples).to_string();
        assert_eq!(text(5, 5), "1");
        assert_eq!(text(0, 5), "0");
        assert_eq!(text(2, 3), "0.67");
        assert_eq!(text(1, 3), "0.33");
        assert_eq!(text(1, 2), "0.5");
    }
    use crate::database::Database;
    use crate::health_bucket::SlotHealth;
    use crate::test_clock;

    fn database_with_phone() -> Database {
        let database = Database::open_in_memory(test_clock::fixed).unwrap();
        database
            .connection
            .execute(
                "INSERT INTO phones (serial, site, last_state, first_seen_at) VALUES ('A', 'home', 'device', '2026-10-21T00:00:00.000Z')",
                [],
            )
            .unwrap();
        database
    }

    fn row(database: &Database) -> String {
        database
            .connection
            .query_row(
                "SELECT bucket || ' ' || samples || ' ' || coalesce(battery_pct, 'null') || ' ' || coalesce(battery_temp_dc, 'null') || ' ' || signal_json || ' ' || service_json || ' ' || coalesce(mobile_rx, 'null') || ' ' || coalesce(mobile_tx, 'null') FROM health_5m",
                [],
                |row| row.get(0),
            )
            .unwrap()
    }

    #[test]
    fn a_bucket_is_written_as_spareline_old_wrote_it() {
        let mut database = database_with_phone();
        let start = HealthBucket::start_of(test_clock::at("2026-10-21T10:07:31.250Z"));
        assert_eq!(start.to_string(), "2026-10-21T10:05:00.000Z");
        let bucket = HealthBucket {
            start,
            samples: 3,
            battery_sum: 172,
            battery_count: 2,
            temp_max: Some(312),
            slots: [
                (
                    1,
                    SlotHealth {
                        min_level: 1,
                        dbm_sum: 0,
                        dbm_count: 0,
                        in_service: 0,
                        samples: 3,
                    },
                ),
                (
                    0,
                    SlotHealth {
                        min_level: 2,
                        dbm_sum: -221,
                        dbm_count: 2,
                        in_service: 2,
                        samples: 3,
                    },
                ),
            ]
            .into_iter()
            .collect(),
        };
        let mobile = MobileBytes { rx: 1000, tx: 20 };
        assert!(database.health().write("A", &bucket, Some(mobile)).unwrap());
        assert_eq!(
            row(&database),
            r#"2026-10-21T10:05:00.000Z 3 86 312 {"0":{"minLevel":2,"dbm":-110},"1":{"minLevel":1,"dbm":null}} {"0":0.67,"1":0} 1000 20"#
        );
        let replaced = HealthBucket {
            samples: 4,
            battery_count: 0,
            temp_max: None,
            slots: [(
                0,
                SlotHealth {
                    min_level: 4,
                    dbm_sum: -90,
                    dbm_count: 1,
                    in_service: 4,
                    samples: 4,
                },
            )]
            .into_iter()
            .collect(),
            ..bucket.clone()
        };
        assert!(database.health().write("A", &replaced, None).unwrap());
        assert_eq!(
            row(&database),
            r#"2026-10-21T10:05:00.000Z 4 null null {"0":{"minLevel":4,"dbm":-90}} {"0":1} null null"#,
            "a row of the same bucket is replaced"
        );
        assert!(
            !database.health().write("B", &bucket, None).unwrap(),
            "an unknown phone writes nothing"
        );
        let empty = HealthBucket {
            samples: 0,
            ..bucket
        };
        assert!(
            !database.health().write("A", &empty, None).unwrap(),
            "an empty bucket writes nothing"
        );
    }

    #[test]
    fn buckets_older_than_90_days_are_pruned() {
        let mut database = database_with_phone();
        for start in ["2026-07-23T00:00:00.000Z", "2026-07-23T00:05:00.000Z"] {
            let bucket = HealthBucket {
                start: test_clock::at(start),
                samples: 1,
                battery_sum: 0,
                battery_count: 0,
                temp_max: None,
                slots: BTreeMap::new(),
            };
            database.health().write("A", &bucket, None).unwrap();
        }
        let now = test_clock::at("2026-10-21T00:02:00.000Z");
        assert_eq!(database.health().prune(now).unwrap(), 1);
        let left: String = database
            .connection
            .query_row("SELECT bucket FROM health_5m", [], |row| row.get(0))
            .unwrap();
        assert_eq!(left, "2026-07-23T00:05:00.000Z");
    }
}
