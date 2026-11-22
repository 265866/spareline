//! The `control_sessions` table: one row per screen viewer that can send input.

use rusqlite::Connection;
use spareline_core::Timestamp;

use crate::database_error::DatabaseError;

/// The audit rows of screen viewers: who watched which phone, when, and how many inputs they
/// sent. Typed text is counted, never stored.
#[derive(Debug)]
pub struct ControlSessions<'db> {
    connection: &'db mut Connection,
}

/// What a viewer sent, counted when it leaves.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ControlCounts {
    /// Inputs sent.
    pub inputs: i64,
    /// Texts typed.
    pub texts: i64,
    /// Control messages the relay refused.
    pub rejected: i64,
}

impl<'db> ControlSessions<'db> {
    pub(crate) fn new(connection: &'db mut Connection) -> Self {
        Self { connection }
    }
}

impl ControlSessions<'_> {
    /// Records a viewer attaching to the phone with `serial`, and returns the row id; `None`
    /// when the phone has no row.
    ///
    /// # Errors
    ///
    /// Fails when SQLite does.
    pub fn open(
        &mut self,
        serial: &str,
        login: Option<&str>,
        now: Timestamp,
    ) -> Result<Option<i64>, DatabaseError> {
        use rusqlite::OptionalExtension as _;
        let phone_id: Option<i64> = self
            .connection
            .query_row("SELECT id FROM phones WHERE serial = ?", [serial], |row| {
                row.get(0)
            })
            .optional()?;
        let Some(phone_id) = phone_id else {
            return Ok(None);
        };
        let id = self.connection.query_row(
            "INSERT INTO control_sessions (phone_id, login, started_at) VALUES (?, ?, ?) RETURNING id",
            (phone_id, login, now.to_string()),
            |row| row.get(0),
        )?;
        Ok(Some(id))
    }

    /// Records the viewer of row `id` leaving, with its counts. A row of a process that died
    /// keeps a null `ended_at`.
    ///
    /// # Errors
    ///
    /// Fails when SQLite does.
    pub fn close(
        &mut self,
        id: i64,
        counts: ControlCounts,
        now: Timestamp,
    ) -> Result<(), DatabaseError> {
        self.connection.execute(
            "UPDATE control_sessions SET ended_at = ?, inputs = ?, texts = ?, rejected = ? WHERE id = ?",
            (now.to_string(), counts.inputs, counts.texts, counts.rejected, id),
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::Database;
    use crate::test_clock;

    #[test]
    fn a_viewer_is_recorded_on_attach_and_counted_on_detach() {
        let mut database = Database::open_in_memory(test_clock::fixed).unwrap();
        database
            .connection
            .execute(
                "INSERT INTO phones (serial, site, last_state, first_seen_at) VALUES ('A', 'home', 'device', '2026-10-21T00:00:00.000Z')",
                [],
            )
            .unwrap();
        let start = test_clock::at("2026-10-21T10:00:00.000Z");
        let end = test_clock::at("2026-10-21T10:30:00.000Z");
        assert_eq!(
            database
                .control_sessions()
                .open("B", Some("alice"), start)
                .unwrap(),
            None
        );
        let id = database
            .control_sessions()
            .open("A", Some("alice"), start)
            .unwrap()
            .unwrap();
        let open: String = database
            .connection
            .query_row(
                "SELECT phone_id || ' ' || login || ' ' || started_at || ' ' || coalesce(ended_at, 'null') || ' ' || inputs || ' ' || texts || ' ' || rejected FROM control_sessions WHERE id = ?",
                [id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(open, "1 alice 2026-10-21T10:00:00.000Z null 0 0 0");
        database
            .control_sessions()
            .close(
                id,
                ControlCounts {
                    inputs: 12,
                    texts: 2,
                    rejected: 1,
                },
                end,
            )
            .unwrap();
        let closed: String = database
            .connection
            .query_row(
                "SELECT ended_at || ' ' || inputs || ' ' || texts || ' ' || rejected FROM control_sessions WHERE id = ?",
                [id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(closed, "2026-10-21T10:30:00.000Z 12 2 1");
        let anonymous = database
            .control_sessions()
            .open("A", None, start)
            .unwrap()
            .unwrap();
        let login: Option<String> = database
            .connection
            .query_row(
                "SELECT login FROM control_sessions WHERE id = ?",
                [anonymous],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(login, None);
    }
}
