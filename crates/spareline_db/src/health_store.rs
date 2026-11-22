//! The `health_5m` table: five-minute health aggregates, written as `spareline_core`'s health
//! tracker closes its in-memory buckets so the SD card sees few writes, and never read by
//! spareline itself.

use rusqlite::Connection;
use spareline_core::Timestamp;
use spareline_core::monitor::HealthRow;

use crate::database_error::DatabaseError;

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
    /// Writes a row `spareline_core`'s health tracker closed, replacing a row of the same
    /// phone and start. Returns whether it wrote: a phone with no row in `phones` writes
    /// nothing, as spareline-old's `HealthRecorder` skipped it.
    ///
    /// # Errors
    ///
    /// Fails when SQLite does.
    pub fn write_row(&mut self, row: &HealthRow) -> Result<bool, DatabaseError> {
        use rusqlite::OptionalExtension as _;
        let phone_id: Option<i64> = self
            .connection
            .query_row(
                "SELECT id FROM phones WHERE serial = ?",
                [&row.serial],
                |found| found.get(0),
            )
            .optional()?;
        let Some(phone_id) = phone_id.filter(|_| row.samples != 0) else {
            return Ok(false);
        };
        self.connection.execute(
            "INSERT OR REPLACE INTO health_5m
                (phone_id, bucket, samples, battery_pct, battery_temp_dc, signal_json, service_json, mobile_rx, mobile_tx)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            rusqlite::params![
                phone_id,
                row.bucket.to_string(),
                i64::try_from(row.samples).unwrap_or(i64::MAX),
                row.battery_pct,
                row.battery_temp_dc,
                row.signal_json,
                row.service_json,
                row.mobile_rx.map(|bytes| i64::try_from(bytes).unwrap_or(i64::MAX)),
                row.mobile_tx.map(|bytes| i64::try_from(bytes).unwrap_or(i64::MAX)),
            ],
        )?;
        Ok(true)
    }

    /// Deletes the buckets that started before `before`, and returns how many there were.
    /// `spareline_core`'s health tracker says when to prune and the cutoff: 90 days back, at
    /// the first sample after start, then at most once a day, as spareline-old did.
    ///
    /// # Errors
    ///
    /// Fails when SQLite does.
    pub fn prune(&mut self, before: Timestamp) -> Result<usize, DatabaseError> {
        let deleted = self.connection.execute(
            "DELETE FROM health_5m WHERE bucket < ?",
            [before.to_string()],
        )?;
        Ok(deleted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::database::Database;
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
    fn a_trackers_row_is_written_as_it_is_and_an_unknown_phone_is_skipped() {
        let mut database = database_with_phone();
        let mut written = HealthRow {
            serial: "A".to_owned(),
            bucket: test_clock::at("2026-10-21T10:05:00.000Z"),
            samples: 3,
            battery_pct: Some(86),
            battery_temp_dc: Some(312),
            signal_json: r#"{"0":{"minLevel":2,"dbm":-110}}"#.to_owned(),
            service_json: r#"{"0":0.67}"#.to_owned(),
            mobile_rx: Some(1000),
            mobile_tx: None,
        };
        assert!(database.health().write_row(&written).unwrap());
        assert_eq!(
            row(&database),
            r#"2026-10-21T10:05:00.000Z 3 86 312 {"0":{"minLevel":2,"dbm":-110}} {"0":0.67} 1000 null"#
        );
        written.samples = 4;
        assert!(database.health().write_row(&written).unwrap());
        assert!(
            row(&database).starts_with("2026-10-21T10:05:00.000Z 4 "),
            "the row was replaced"
        );
        written.serial = "B".to_owned();
        assert!(
            !database.health().write_row(&written).unwrap(),
            "no phone B"
        );
    }

    fn row_at(bucket: &str, samples: u64) -> HealthRow {
        HealthRow {
            serial: "A".to_owned(),
            bucket: test_clock::at(bucket),
            samples,
            battery_pct: None,
            battery_temp_dc: None,
            signal_json: "{}".to_owned(),
            service_json: "{}".to_owned(),
            mobile_rx: None,
            mobile_tx: None,
        }
    }

    #[test]
    fn an_empty_bucket_writes_nothing() {
        let mut database = database_with_phone();
        assert!(
            !database
                .health()
                .write_row(&row_at("2026-10-21T10:05:00.000Z", 0))
                .unwrap(),
            "an empty bucket writes nothing"
        );
        let count: i64 = database
            .connection
            .query_row("SELECT count(*) FROM health_5m", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn buckets_before_the_cutoff_are_pruned() {
        let mut database = database_with_phone();
        for start in ["2026-07-23T00:00:00.000Z", "2026-07-23T00:05:00.000Z"] {
            database.health().write_row(&row_at(start, 1)).unwrap();
        }
        let cutoff = test_clock::at("2026-07-23T00:02:00.000Z");
        assert_eq!(database.health().prune(cutoff).unwrap(), 1);
        let left: String = database
            .connection
            .query_row("SELECT bucket FROM health_5m", [], |row| row.get(0))
            .unwrap();
        assert_eq!(left, "2026-07-23T00:05:00.000Z");
    }
}
