//! Phones, SIMs and numbers: the queries behind the inventory API and the edits people make.

use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension as _, Row};
use serde::Serialize;
use spareline_core::Timestamp;

use crate::database_error::DatabaseError;
use crate::field_patch::FieldPatch;
use crate::inventory_error::InventoryError;
use crate::number_record::{LastMessage, NumberPatch, NumberPhone, NumberRecord};
use crate::number_summary::{NumberSource, NumberSummary};
use crate::phone_record::{PhonePatch, PhoneRecord, PlacementRecord};
use crate::sim_record::{LastPlacement, SimPatch, SimPlacement, SimRecord};
use crate::sim_summary::SimSummary;

const SELECT_PHONES: &str = "SELECT id, serial, site, manufacturer, model, android_version, sdk, imei1, imei2, location, hub_port, cost_cents, acquired_on, last_state, first_seen_at, last_online_at FROM phones";

/// Phones, SIMs, placements, numbers and bindings. The writes that probes make are in
/// [`Inventory::record_phone_state`] and [`Inventory::apply_identity`].
#[derive(Debug)]
pub struct Inventory<'db> {
    pub(crate) connection: &'db mut Connection,
}

impl<'db> Inventory<'db> {
    pub(crate) fn new(connection: &'db mut Connection) -> Self {
        Self { connection }
    }
}

impl Inventory<'_> {
    /// Every phone, ordered by serial.
    ///
    /// # Errors
    ///
    /// Fails when SQLite does.
    pub fn list_phones(&self) -> Result<Vec<PhoneRecord>, DatabaseError> {
        let mut statement = self
            .connection
            .prepare(&format!("{SELECT_PHONES} ORDER BY serial"))?;
        let rows = statement
            .query_map([], phone_from)?
            .collect::<Result<Vec<PhoneRecord>, _>>()?;
        rows.into_iter()
            .map(|phone| self.with_placements(phone))
            .collect()
    }

    /// The phone with `serial`.
    ///
    /// # Errors
    ///
    /// Fails when SQLite does.
    pub fn phone(&self, serial: &str) -> Result<Option<PhoneRecord>, DatabaseError> {
        let row = self
            .connection
            .query_row(
                &format!("{SELECT_PHONES} WHERE serial = ?"),
                [serial],
                phone_from,
            )
            .optional()?;
        row.map(|phone| self.with_placements(phone)).transpose()
    }

    /// The row id of the phone with `serial`.
    ///
    /// # Errors
    ///
    /// Fails when SQLite does.
    pub fn phone_id(&self, serial: &str) -> Result<Option<i64>, DatabaseError> {
        let id = self
            .connection
            .query_row("SELECT id FROM phones WHERE serial = ?", [serial], |row| {
                row.get(0)
            })
            .optional()?;
        Ok(id)
    }

    /// The serials of phones last seen in a settled state other than `gone` and `offline`,
    /// which the device manager marks gone when the adb server no longer lists them, so a
    /// restart doesn't leave a stale state.
    ///
    /// # Errors
    ///
    /// Fails when SQLite does.
    pub fn serials_not_gone(&self) -> Result<Vec<String>, DatabaseError> {
        let mut statement = self
            .connection
            .prepare("SELECT serial FROM phones WHERE last_state NOT IN ('gone', 'offline')")?;
        let serials = statement
            .query_map([], |row| row.get(0))?
            .collect::<Result<Vec<String>, _>>()?;
        Ok(serials)
    }

    /// Writes the fields `patch` sets on the phone with `serial`, and an `inventory_edits` row
    /// with `login` and the patch, in one transaction.
    ///
    /// # Errors
    ///
    /// Fails with [`InventoryError::NoPhone`] for an unknown serial, and when SQLite fails.
    pub fn patch_phone(
        &mut self,
        serial: &str,
        patch: &PhonePatch,
        login: Option<&str>,
        now: Timestamp,
    ) -> Result<PhoneRecord, InventoryError> {
        let phone = self.phone(serial)?.ok_or_else(|| InventoryError::NoPhone {
            serial: serial.to_owned(),
        })?;
        let mut columns: Vec<Column> = Vec::new();
        push_text(&mut columns, "location", &patch.location);
        push_text(&mut columns, "hub_port", &patch.hub_port);
        if !patch.cost_cents.is_absent() {
            columns.push(Column {
                name: "cost_cents",
                value: patch
                    .cost_cents
                    .as_option()
                    .map_or(Value::Null, |cents| Value::Integer(*cents)),
            });
        }
        push_text(&mut columns, "acquired_on", &patch.acquired_on);
        let transaction = self.connection.transaction()?;
        update(&transaction, "phones", phone.id, columns)?;
        audit(&transaction, login, "phone", phone.id, patch, now)?;
        transaction.commit()?;
        self.phone(serial)?.ok_or_else(|| InventoryError::NoPhone {
            serial: serial.to_owned(),
        })
    }

    /// Every SIM, ordered by id.
    ///
    /// # Errors
    ///
    /// Fails when SQLite does.
    pub fn list_sims(&self) -> Result<Vec<SimRecord>, DatabaseError> {
        let mut statement = self.connection.prepare("SELECT id FROM sims ORDER BY id")?;
        let ids = statement
            .query_map([], |row| row.get(0))?
            .collect::<Result<Vec<i64>, _>>()?;
        let mut sims = Vec::with_capacity(ids.len());
        for id in ids {
            sims.extend(self.sim(id)?);
        }
        Ok(sims)
    }

    /// The SIM with the id.
    ///
    /// # Errors
    ///
    /// Fails when SQLite does.
    pub fn sim(&self, id: i64) -> Result<Option<SimRecord>, DatabaseError> {
        let sim = self
            .connection
            .query_row(
                "SELECT id, iccid, carrier, status, first_seen_at, last_seen_at FROM sims WHERE id = ?",
                [id],
                |row| {
                    Ok(SimRecord {
                        id: row.get(0)?,
                        iccid: row.get(1)?,
                        carrier: row.get(2)?,
                        status: row.get(3)?,
                        first_seen_at: row.get(4)?,
                        last_seen_at: row.get(5)?,
                        placement: None,
                        last_placement: None,
                        number: None,
                    })
                },
            )
            .optional()?;
        let Some(mut sim) = sim else {
            return Ok(None);
        };
        sim.placement = self
            .connection
            .query_row(
                "SELECT p.phone_id, ph.serial, p.slot, p.sub_id, p.from_at FROM sim_placements p JOIN phones ph ON ph.id = p.phone_id
                WHERE p.sim_id = ? AND p.to_at IS NULL",
                [id],
                |row| {
                    Ok(SimPlacement {
                        phone_id: row.get(0)?,
                        serial: row.get(1)?,
                        slot: row.get(2)?,
                        sub_id: row.get(3)?,
                        since: row.get(4)?,
                    })
                },
            )
            .optional()?;
        if sim.placement.is_none() {
            sim.last_placement = self
                .connection
                .query_row(
                    "SELECT ph.serial, p.slot, p.to_at FROM sim_placements p JOIN phones ph ON ph.id = p.phone_id
                    WHERE p.sim_id = ? ORDER BY p.to_at DESC LIMIT 1",
                    [id],
                    |row| {
                        Ok(LastPlacement {
                            serial: row.get(0)?,
                            slot: row.get(1)?,
                            until: row.get(2)?,
                        })
                    },
                )
                .optional()?;
        }
        sim.number = number_of_sim(self.connection, id)?;
        Ok(Some(sim))
    }

    /// The SIM `key` names: up to 9 digits are its id, anything else its ICCID.
    ///
    /// # Errors
    ///
    /// Fails when SQLite does.
    pub fn find_sim(&self, key: &str) -> Result<Option<SimRecord>, DatabaseError> {
        let id = match short_id(key) {
            Some(id) => Some(id),
            None => self
                .connection
                .query_row("SELECT id FROM sims WHERE iccid = ?", [key], |row| {
                    row.get(0)
                })
                .optional()?,
        };
        match id {
            Some(id) => self.sim(id),
            None => Ok(None),
        }
    }

    /// Writes the fields `patch` sets on the SIM, and an `inventory_edits` row, in one
    /// transaction.
    ///
    /// # Errors
    ///
    /// Fails with [`InventoryError::NoSim`] for an unknown id, and when SQLite fails.
    pub fn patch_sim(
        &mut self,
        id: i64,
        patch: &SimPatch,
        login: Option<&str>,
        now: Timestamp,
    ) -> Result<SimRecord, InventoryError> {
        if self.sim(id)?.is_none() {
            return Err(InventoryError::NoSim { id });
        }
        let mut columns = Vec::new();
        if let Some(status) = patch.status {
            columns.push(Column {
                name: "status",
                value: Value::Text(status.as_str().to_owned()),
            });
        }
        let transaction = self.connection.transaction()?;
        update(&transaction, "sims", id, columns)?;
        audit(&transaction, login, "sim", id, patch, now)?;
        transaction.commit()?;
        self.sim(id)?.ok_or(InventoryError::NoSim { id })
    }

    /// Every number, those with an E.164 value first in its order, then by id.
    ///
    /// # Errors
    ///
    /// Fails when SQLite does.
    pub fn list_numbers(&self) -> Result<Vec<NumberRecord>, DatabaseError> {
        let mut statement = self
            .connection
            .prepare("SELECT id FROM numbers ORDER BY e164 IS NULL, e164, id")?;
        let ids = statement
            .query_map([], |row| row.get(0))?
            .collect::<Result<Vec<i64>, _>>()?;
        let mut numbers = Vec::with_capacity(ids.len());
        for id in ids {
            numbers.extend(self.number(id)?);
        }
        Ok(numbers)
    }

    /// The number `key` names: its E.164 value when the key starts with `+`, its id when the
    /// key is up to 9 digits.
    ///
    /// # Errors
    ///
    /// Fails when SQLite does.
    pub fn find_number(&self, key: &str) -> Result<Option<NumberRecord>, DatabaseError> {
        match self.number_id_of(key)? {
            Some(id) => self.number(id),
            None => Ok(None),
        }
    }

    /// The id of the number `key` names, read as [`find_number`](Self::find_number) reads it.
    ///
    /// # Errors
    ///
    /// Fails when SQLite does.
    pub fn number_id_of(&self, key: &str) -> Result<Option<i64>, DatabaseError> {
        let id = if key.starts_with('+') {
            self.connection
                .query_row("SELECT id FROM numbers WHERE e164 = ?", [key], |row| {
                    row.get(0)
                })
                .optional()?
        } else if let Some(id) = short_id(key) {
            self.connection
                .query_row("SELECT id FROM numbers WHERE id = ?", [id], |row| {
                    row.get(0)
                })
                .optional()?
        } else {
            None
        };
        Ok(id)
    }

    /// The number with the id.
    ///
    /// # Errors
    ///
    /// Fails when SQLite does.
    pub fn number(&self, id: i64) -> Result<Option<NumberRecord>, DatabaseError> {
        let number = self
            .connection
            .query_row(
                "SELECT id, e164, e164_source, note, created_at FROM numbers WHERE id = ?",
                [id],
                |row| {
                    Ok(NumberRecord {
                        id: row.get(0)?,
                        e164: row.get(1)?,
                        source: row.get(2)?,
                        note: row.get(3)?,
                        detected_e164: None,
                        confirmed_by: None,
                        created_at: row.get(4)?,
                        sim: None,
                        phone: None,
                        message_count: 0,
                        last_message: None,
                    })
                },
            )
            .optional()?;
        let Some(mut number) = number else {
            return Ok(None);
        };
        let binding = self
            .connection
            .query_row(
                "SELECT b.sim_id, b.detected_e164, b.confirmed_by, s.iccid, s.carrier FROM number_bindings b JOIN sims s ON s.id = b.sim_id
                WHERE b.number_id = ? ORDER BY b.to_at IS NOT NULL, b.from_at DESC LIMIT 1",
                [id],
                |row| {
                    Ok(Binding {
                        detected_e164: row.get(1)?,
                        confirmed_by: row.get(2)?,
                        sim: SimSummary {
                            id: row.get(0)?,
                            iccid: row.get(3)?,
                            carrier: row.get(4)?,
                        },
                    })
                },
            )
            .optional()?;
        if let Some(binding) = binding {
            number.phone = self
                .connection
                .query_row(
                    "SELECT ph.serial, ph.model, p.slot, p.to_at FROM sim_placements p JOIN phones ph ON ph.id = p.phone_id
                    WHERE p.sim_id = ? ORDER BY p.to_at IS NOT NULL, p.from_at DESC LIMIT 1",
                    [binding.sim.id],
                    |row| {
                        Ok(NumberPhone {
                            serial: row.get(0)?,
                            model: row.get(1)?,
                            slot: row.get(2)?,
                            removed_at: row.get(3)?,
                        })
                    },
                )
                .optional()?;
            number.detected_e164 = binding.detected_e164;
            number.confirmed_by = binding.confirmed_by;
            number.sim = Some(binding.sim);
        }
        let last = self
            .connection
            .query_row(
                "SELECT (SELECT count(*) FROM messages WHERE number_id = $id) AS count,
                seq, sender, substr(body, 1, 160) AS preview, received_at
                FROM messages WHERE number_id = $id ORDER BY seq DESC LIMIT 1",
                rusqlite::named_params! { "$id": id },
                |row| {
                    Ok(CountedMessage {
                        count: row.get(0)?,
                        last: LastMessage {
                            seq: row.get(1)?,
                            from: row.get(2)?,
                            preview: row.get(3)?,
                            received_at: row.get(4)?,
                        },
                    })
                },
            )
            .optional()?;
        if let Some(last) = last {
            number.message_count = last.count;
            number.last_message = Some(last.last);
        }
        Ok(Some(number))
    }

    /// Writes the note and the E.164 value `patch` sets on the number, and an
    /// `inventory_edits` row, in one transaction. A `null` E.164 returns the number to what its
    /// SIM reports, or to none.
    ///
    /// Setting `e164` updates `confirmed_by` on the open binding to `login`, or `unknown`
    /// without one, and clears it for `null`.
    ///
    /// # Errors
    ///
    /// Fails with [`InventoryError::NoNumber`] for an unknown id, with
    /// [`InventoryError::Conflict`] when another number has the value, and when SQLite fails.
    /// Nothing changes then.
    pub fn patch_number(
        &mut self,
        id: i64,
        patch: &NumberPatch,
        login: Option<&str>,
        now: Timestamp,
    ) -> Result<NumberRecord, InventoryError> {
        let number = self.number(id)?.ok_or(InventoryError::NoNumber { id })?;
        let transaction = self.connection.transaction()?;
        if let Some(note) = &patch.note {
            update(
                &transaction,
                "numbers",
                id,
                vec![Column {
                    name: "note",
                    value: Value::Text(note.clone()),
                }],
            )?;
        }
        if !patch.e164.is_absent() {
            let target = patch
                .e164
                .as_option()
                .cloned()
                .or_else(|| number.detected_e164.clone());
            if let Some(target) = &target {
                let owner: Option<i64> = transaction
                    .query_row("SELECT id FROM numbers WHERE e164 = ?", [target], |row| {
                        row.get(0)
                    })
                    .optional()?;
                if let Some(owner) = owner.filter(|owner| *owner != id) {
                    return Err(InventoryError::Conflict {
                        e164: target.clone(),
                        owner,
                    });
                }
            }
            let source = match (&patch.e164, &target) {
                (FieldPatch::Value(_), _) => NumberSource::Manual,
                (_, None) => NumberSource::None,
                (_, Some(_)) => NumberSource::Detected,
            };
            transaction.execute(
                "UPDATE numbers SET e164 = ?, e164_source = ? WHERE id = ?",
                (&target, source, id),
            )?;
            let confirmed_by = match &patch.e164 {
                FieldPatch::Null => None,
                FieldPatch::Absent | FieldPatch::Value(_) => Some(login.unwrap_or("unknown")),
            };
            transaction.execute(
                "UPDATE number_bindings SET confirmed_by = ? WHERE number_id = ? AND to_at IS NULL",
                (confirmed_by, id),
            )?;
        }
        audit(&transaction, login, "number", id, patch, now)?;
        transaction.commit()?;
        self.number(id)?.ok_or(InventoryError::NoNumber { id })
    }

    /// `phone` with the SIMs placed in it now.
    fn with_placements(&self, mut phone: PhoneRecord) -> Result<PhoneRecord, DatabaseError> {
        let mut statement = self.connection.prepare(
            "SELECT p.slot, p.sub_id, p.from_at, s.id AS sim_id, s.iccid, s.carrier FROM sim_placements p
            JOIN sims s ON s.id = p.sim_id WHERE p.phone_id = ? AND p.to_at IS NULL ORDER BY p.slot",
        )?;
        let placements = statement
            .query_map([phone.id], |row| {
                Ok(PlacementRecord {
                    slot: row.get(0)?,
                    sub_id: row.get(1)?,
                    since: row.get(2)?,
                    sim: SimSummary {
                        id: row.get(3)?,
                        iccid: row.get(4)?,
                        carrier: row.get(5)?,
                    },
                    number: None,
                })
            })?
            .collect::<Result<Vec<PlacementRecord>, _>>()?;
        for mut placement in placements {
            placement.number = number_of_sim(self.connection, placement.sim.id)?;
            phone.placements.push(placement);
        }
        Ok(phone)
    }
}

/// The number the SIM's open binding names.
fn number_of_sim(
    connection: &Connection,
    sim_id: i64,
) -> Result<Option<NumberSummary>, DatabaseError> {
    let number = connection
        .query_row(
            "SELECT n.id, n.e164, n.e164_source, n.note FROM number_bindings b JOIN numbers n ON n.id = b.number_id
            WHERE b.sim_id = ? AND b.to_at IS NULL",
            [sim_id],
            |row| {
                Ok(NumberSummary {
                    id: row.get(0)?,
                    e164: row.get(1)?,
                    source: row.get(2)?,
                    note: row.get(3)?,
                })
            },
        )
        .optional()?;
    Ok(number)
}

/// The id a key of up to 9 digits names, as `/^\d{1,9}$/` and `Number(key)` read it.
fn short_id(key: &str) -> Option<i64> {
    if (1..=9).contains(&key.len()) && key.bytes().all(|byte| byte.is_ascii_digit()) {
        key.parse().ok()
    } else {
        None
    }
}

/// A column to set, and its value.
struct Column {
    name: &'static str,
    value: Value,
}

fn push_text(columns: &mut Vec<Column>, name: &'static str, field: &FieldPatch<String>) {
    if !field.is_absent() {
        columns.push(Column {
            name,
            value: field
                .as_option()
                .map_or(Value::Null, |text| Value::Text(text.clone())),
        });
    }
}

/// Sets `columns` on the row `id` of `table`; nothing when there are none.
fn update(
    connection: &Connection,
    table: &str,
    id: i64,
    columns: Vec<Column>,
) -> Result<(), DatabaseError> {
    if columns.is_empty() {
        return Ok(());
    }
    let set: Vec<String> = columns
        .iter()
        .map(|column| format!("{} = ?", column.name))
        .collect();
    let mut values: Vec<Value> = columns.into_iter().map(|column| column.value).collect();
    values.push(Value::Integer(id));
    connection.execute(
        &format!("UPDATE {table} SET {} WHERE id = ?", set.join(", ")),
        rusqlite::params_from_iter(values),
    )?;
    Ok(())
}

/// Records an edit people made: who, which row, and the patch as JSON.
fn audit(
    connection: &Connection,
    login: Option<&str>,
    entity: &str,
    entity_id: i64,
    changes: &impl Serialize,
    now: Timestamp,
) -> Result<(), DatabaseError> {
    let changes = serde_json::to_string(changes)
        .map_err(|source| DatabaseError::json(format!("the {entity} edit"), source))?;
    connection.execute(
        "INSERT INTO inventory_edits (at, login, entity, entity_id, changes_json) VALUES (?, ?, ?, ?, ?)",
        (now.to_string(), login, entity, entity_id, changes),
    )?;
    Ok(())
}

/// The binding a number shows: the open one, else the latest.
struct Binding {
    detected_e164: Option<String>,
    confirmed_by: Option<String>,
    sim: SimSummary,
}

struct CountedMessage {
    count: i64,
    last: LastMessage,
}

fn phone_from(row: &Row<'_>) -> rusqlite::Result<PhoneRecord> {
    let imei1: Option<String> = row.get(7)?;
    let imei2: Option<String> = row.get(8)?;
    Ok(PhoneRecord {
        id: row.get(0)?,
        serial: row.get(1)?,
        site: row.get(2)?,
        manufacturer: row.get(3)?,
        model: row.get(4)?,
        android_version: row.get(5)?,
        sdk: row.get(6)?,
        imeis: imei1.into_iter().chain(imei2).collect(),
        location: row.get(9)?,
        hub_port: row.get(10)?,
        cost_cents: row.get(11)?,
        acquired_on: row.get(12)?,
        last_state: row.get(13)?,
        first_seen_at: row.get(14)?,
        last_online_at: row.get(15)?,
        placements: Vec::new(),
    })
}
