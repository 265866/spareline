//! The `device_events` table: recording events and replaying them for the event stream.

use rusqlite::{Connection, OptionalExtension as _, Row};
use serde::Deserialize;
use serde_json::value::RawValue;
use spareline_core::Timestamp;

use crate::database_error::DatabaseError;
use crate::device_event::{DeviceEvent, LastProvision, NewEvent};

const SELECT_EVENTS: &str = "SELECT e.id, e.phone_id, p.serial, e.at, e.kind, e.detail_json FROM device_events e LEFT JOIN phones p ON p.id = e.phone_id";

/// The device event log.
#[derive(Debug)]
pub struct DeviceEvents<'db> {
    connection: &'db mut Connection,
}

impl<'db> DeviceEvents<'db> {
    pub(crate) fn new(connection: &'db mut Connection) -> Self {
        Self { connection }
    }
}

impl DeviceEvents<'_> {
    /// Records `events`, all at `at`, in one transaction, and returns them as the stream
    /// publishes them, with their ids and their phones' serials.
    ///
    /// # Errors
    ///
    /// Fails when SQLite does; then none is recorded.
    pub fn insert(
        &mut self,
        events: &[NewEvent],
        at: Timestamp,
    ) -> Result<Vec<DeviceEvent>, DatabaseError> {
        let at = at.to_string();
        let transaction = self.connection.transaction()?;
        let mut recorded = Vec::with_capacity(events.len());
        {
            let mut insert = transaction.prepare(
                "INSERT INTO device_events (phone_id, at, kind, detail_json) VALUES (?, ?, ?, ?) RETURNING id",
            )?;
            let mut serial_of = transaction.prepare("SELECT serial FROM phones WHERE id = ?")?;
            for event in events {
                let id: i64 = insert.query_row(
                    (event.phone_id, &at, &event.kind, event.detail.get()),
                    |row| row.get(0),
                )?;
                let serial = match event.phone_id {
                    Some(phone_id) => serial_of
                        .query_row([phone_id], |row| row.get(0))
                        .optional()?,
                    None => None,
                };
                recorded.push(DeviceEvent {
                    id,
                    phone_id: event.phone_id,
                    serial,
                    at: at.clone(),
                    kind: event.kind.clone(),
                    detail: event.detail.clone(),
                });
            }
        }
        transaction.commit()?;
        Ok(recorded)
    }

    /// Up to `limit` events with ids above `after`, oldest first: the stream's replay from a
    /// cursor.
    ///
    /// # Errors
    ///
    /// Fails when SQLite does, or a stored detail isn't JSON.
    pub fn after(&self, after: i64, limit: i64) -> Result<Vec<DeviceEvent>, DatabaseError> {
        self.query("WHERE e.id > ? ORDER BY e.id ASC LIMIT ?", after, limit)
    }

    /// The newest `count` events, oldest first: the stream's `latest=N` replay.
    ///
    /// # Errors
    ///
    /// Fails when SQLite does, or a stored detail isn't JSON.
    pub fn latest(&self, count: i64) -> Result<Vec<DeviceEvent>, DatabaseError> {
        let mut events = self.query("WHERE e.id > ? ORDER BY e.id DESC LIMIT ?", 0, count)?;
        events.reverse();
        Ok(events)
    }

    /// The newest event's id, or 0 when there is none: where a stream without a cursor starts.
    ///
    /// # Errors
    ///
    /// Fails when SQLite does.
    pub fn latest_id(&self) -> Result<i64, DatabaseError> {
        let id: Option<i64> = self
            .connection
            .query_row(
                "SELECT e.id FROM device_events e WHERE e.id > 0 ORDER BY e.id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        Ok(id.unwrap_or(0))
    }

    /// The last `provision` event of the phone with `serial`.
    ///
    /// # Errors
    ///
    /// Fails when SQLite does, or the stored detail isn't JSON of the shape spareline-old
    /// wrote.
    pub fn last_provision(&self, serial: &str) -> Result<Option<LastProvision>, DatabaseError> {
        let row: Option<StoredProvision> = self
            .connection
            .query_row(
                "SELECT e.at, e.detail_json FROM device_events e JOIN phones p ON p.id = e.phone_id
                WHERE p.serial = ? AND e.kind = 'provision' ORDER BY e.id DESC LIMIT 1",
                [serial],
                |row| {
                    Ok(StoredProvision {
                        at: row.get(0)?,
                        detail_json: row.get(1)?,
                    })
                },
            )
            .optional()?;
        let Some(row) = row else {
            return Ok(None);
        };
        let detail: ProvisionDetail = serde_json::from_str(&row.detail_json).map_err(|source| {
            DatabaseError::json(format!("the provision event of {serial}"), source)
        })?;
        Ok(Some(LastProvision {
            at: row.at,
            ok: detail.ok.unwrap_or(false),
            changed: detail.changed.unwrap_or_default(),
            duration_ms: detail.duration_ms.unwrap_or(0),
            error: detail.error,
        }))
    }

    fn query(
        &self,
        clause: &str,
        after: i64,
        limit: i64,
    ) -> Result<Vec<DeviceEvent>, DatabaseError> {
        let mut statement = self
            .connection
            .prepare(&format!("{SELECT_EVENTS} {clause}"))?;
        let rows = statement
            .query_map((after, limit), stored_event_from)?
            .collect::<Result<Vec<StoredEvent>, _>>()?;
        let mut events = Vec::with_capacity(rows.len());
        for row in rows {
            let detail = RawValue::from_string(row.detail_json).map_err(|source| {
                DatabaseError::json(format!("detail_json of device event {}", row.id), source)
            })?;
            events.push(DeviceEvent {
                id: row.id,
                phone_id: row.phone_id,
                serial: row.serial,
                at: row.at,
                kind: row.kind,
                detail,
            });
        }
        Ok(events)
    }
}

/// A `device_events` row before its detail is checked.
struct StoredEvent {
    id: i64,
    phone_id: Option<i64>,
    serial: Option<String>,
    at: String,
    kind: String,
    detail_json: String,
}

fn stored_event_from(row: &Row<'_>) -> rusqlite::Result<StoredEvent> {
    Ok(StoredEvent {
        id: row.get(0)?,
        phone_id: row.get(1)?,
        serial: row.get(2)?,
        at: row.get(3)?,
        kind: row.get(4)?,
        detail_json: row.get(5)?,
    })
}

struct StoredProvision {
    at: String,
    detail_json: String,
}

/// The fields of a `provision` event's detail that spareline-old read back.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProvisionDetail {
    ok: Option<bool>,
    changed: Option<Vec<String>>,
    duration_ms: Option<i64>,
    error: Option<String>,
}

#[cfg(test)]
mod tests {
    use serde::Serialize;

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

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Provision<'detail> {
        ok: bool,
        changed: &'detail [&'detail str],
        failed_commands: &'detail [&'detail str],
        failed_rules: &'detail [&'detail str],
        duration_ms: i64,
        error: Option<&'detail str>,
    }

    #[test]
    fn events_are_recorded_with_serials_and_replayed_in_order() {
        let mut database = database_with_phone();
        let at = test_clock::at("2026-11-22T07:32:46.343Z");
        let recorded = database
            .device_events()
            .insert(
                &[
                    NewEvent::adb_unreachable(
                        "cannot reach the adb server at 127.0.0.1:1: Error: Failed to connect",
                    )
                    .unwrap(),
                    NewEvent::adb_reachable().unwrap(),
                ],
                at,
            )
            .unwrap();
        assert_eq!(
            serde_json::to_string(&recorded[0]).unwrap(),
            r#"{"id":1,"phoneId":null,"serial":null,"at":"2026-11-22T07:32:46.343Z","kind":"adb.unreachable","detail":{"error":"cannot reach the adb server at 127.0.0.1:1: Error: Failed to connect"}}"#
        );
        assert_eq!(recorded[1].detail.get(), r#"{"error":null}"#);
        let mut many = Vec::new();
        for index in 0..5 {
            many.push(
                NewEvent::try_new(
                    Some(1),
                    "alert.cleared",
                    &serde_json::json!({ "key": format!("offline:A:{index}"), "kind": "offline" }),
                )
                .unwrap(),
            );
        }
        let rows = database.device_events().insert(&many, at).unwrap();
        assert_eq!(rows[0].serial.as_deref(), Some("A"));
        let events = database.device_events();
        let ids = |list: Vec<DeviceEvent>| list.iter().map(|event| event.id).collect::<Vec<i64>>();
        assert_eq!(ids(events.after(2, 3).unwrap()), [3, 4, 5]);
        assert_eq!(ids(events.after(6, 500).unwrap()), [7]);
        assert_eq!(
            ids(events.latest(3).unwrap()),
            [5, 6, 7],
            "the newest, oldest first"
        );
        assert_eq!(events.latest_id().unwrap(), 7);
        let replayed = events.after(0, 1).unwrap();
        assert_eq!(
            serde_json::to_string(&replayed[0]).unwrap(),
            serde_json::to_string(&recorded[0]).unwrap(),
            "a replayed event serves what was published"
        );
        let stored: String = database
            .connection
            .query_row(
                "SELECT detail_json FROM device_events WHERE id = 3",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, r#"{"key":"offline:A:0","kind":"offline"}"#);
    }

    #[test]
    fn an_empty_log_starts_at_zero() {
        let mut database = database_with_phone();
        assert_eq!(database.device_events().latest_id().unwrap(), 0);
        assert!(database.device_events().latest(20).unwrap().is_empty());
        assert_eq!(database.device_events().last_provision("A").unwrap(), None);
    }

    #[test]
    fn the_last_provision_is_read_back_with_defaults() {
        let mut database = database_with_phone();
        let first = Provision {
            ok: false,
            changed: &[],
            failed_commands: &[],
            failed_rules: &["lock_screen"],
            duration_ms: 1200,
            error: Some("boom"),
        };
        let second = Provision {
            ok: true,
            changed: &["stay_awake", "wifi_off"],
            failed_commands: &[],
            failed_rules: &[],
            duration_ms: 3105,
            error: None,
        };
        let mut events = database.device_events();
        events
            .insert(
                &[NewEvent::try_new(Some(1), "provision", &first).unwrap()],
                test_clock::at("2026-10-21T00:00:00.000Z"),
            )
            .unwrap();
        events
            .insert(
                &[NewEvent::try_new(Some(1), "provision", &second).unwrap()],
                test_clock::at("2026-10-22T00:00:00.000Z"),
            )
            .unwrap();
        events
            .insert(
                &[NewEvent::try_new(Some(1), "drift", &serde_json::json!({})).unwrap()],
                test_clock::at("2026-10-23T00:00:00.000Z"),
            )
            .unwrap();
        assert_eq!(
            events.last_provision("A").unwrap(),
            Some(LastProvision {
                at: "2026-10-22T00:00:00.000Z".to_owned(),
                ok: true,
                changed: vec!["stay_awake".to_owned(), "wifi_off".to_owned()],
                duration_ms: 3105,
                error: None,
            })
        );
        events
            .insert(
                &[NewEvent::try_new(Some(1), "provision", &serde_json::json!({})).unwrap()],
                test_clock::at("2026-10-24T00:00:00.000Z"),
            )
            .unwrap();
        assert_eq!(
            events.last_provision("A").unwrap(),
            Some(LastProvision {
                at: "2026-10-24T00:00:00.000Z".to_owned(),
                ok: false,
                changed: Vec::new(),
                duration_ms: 0,
                error: None,
            })
        );
        assert_eq!(events.last_provision("B").unwrap(), None);
    }
}
