//! The inventory writes that adb states and identity probes make, as spareline-old's
//! `inventory/sync.ts` did.

use rusqlite::{Connection, OptionalExtension as _, Row};
use serde::Serialize;
use spareline_core::Timestamp;

use crate::database_error::DatabaseError;
use crate::device_event::NewEvent;
use crate::identity_reading::{IdentityReading, SimReading, SlotContents};
use crate::inventory::Inventory;
use crate::number_summary::NumberSource;
use crate::phone_state::PhoneStatus;
use crate::phone_state_update::{PhoneStateUpdate, RecordedState};

impl Inventory<'_> {
    /// Records a phone's adb state. The phone row is created on first sight, in any state, so
    /// a phone waiting for "Allow USB debugging" shows up at once, with a `phone.added` event.
    ///
    /// The transient states `connecting` and `authorizing` are neither stored nor events, so a
    /// replug yields one offline and one online event, each compared against the last settled
    /// state. A change of status yields `phone.online`, `phone.needs_auth`,
    /// `phone.no_permissions`, `phone.offline` or `phone.other_state`, with
    /// `{"state", "rawState"?}`. `last_online_at` moves on every `device` state.
    ///
    /// # Errors
    ///
    /// Fails when SQLite does; then nothing changes.
    pub fn record_phone_state(
        &mut self,
        update: &PhoneStateUpdate,
    ) -> Result<RecordedState, DatabaseError> {
        let transaction = self.connection.transaction()?;
        let at = update.at.to_string();
        let state = update.state.as_str();
        let existing: Option<ExistingPhone> = transaction
            .query_row(
                "SELECT id, last_state FROM phones WHERE serial = ?",
                [&update.serial],
                |row| {
                    Ok(ExistingPhone {
                        id: row.get(0)?,
                        last_state: row.get(1)?,
                    })
                },
            )
            .optional()?;
        let online = (update.state.status() == PhoneStatus::Online).then_some(at.as_str());
        let after = update.state.status();
        let mut events = Vec::new();
        let (phone_id, before) = if let Some(existing) = existing {
            if after != PhoneStatus::Connecting {
                transaction.execute(
                        "UPDATE phones SET last_state = ?, last_online_at = coalesce(?, last_online_at) WHERE id = ?",
                        (state, online, existing.id),
                    )?;
            }
            (
                existing.id,
                Some(PhoneStatus::of_state(&existing.last_state)),
            )
        } else {
            let id: i64 = transaction.query_row(
                    "INSERT INTO phones (serial, site, last_state, first_seen_at, last_online_at) VALUES (?, ?, ?, ?, ?) RETURNING id",
                    (&update.serial, &update.site, state, &at, online),
                    |row| row.get(0),
                )?;
            events.push(NewEvent::try_new(
                Some(id),
                "phone.added",
                &PhoneAdded {
                    serial: &update.serial,
                    state,
                },
            )?);
            (id, None)
        };
        if Some(after) != before && after != PhoneStatus::Connecting {
            let kind = match after {
                PhoneStatus::Online => "phone.online",
                PhoneStatus::NeedsAuth => "phone.needs_auth",
                PhoneStatus::NoPermissions => "phone.no_permissions",
                PhoneStatus::Offline | PhoneStatus::Connecting => "phone.offline",
                PhoneStatus::Other => "phone.other_state",
            };
            events.push(NewEvent::try_new(
                Some(phone_id),
                kind,
                &StateChanged {
                    state,
                    raw_state: update.raw_state.as_deref(),
                },
            )?);
        }
        transaction.commit()?;
        Ok(RecordedState { phone_id, events })
    }

    /// Applies a fresh identity probe of the phone: its device facts, which SIM is in which
    /// slot, and which number each SIM carries, in one transaction. Returns the events, in
    /// order: `sim.added`, `sim.removed`, `sim.placed`, `number.bound`, `number.detected` and
    /// `number.changed`, with spareline-old's details.
    ///
    /// Device facts the probe couldn't read keep their stored values. A slot in an unknown
    /// state is left as it was, so a SIM that is still loading isn't recorded as removed. A SIM
    /// found in another phone or slot has its old placement closed there. A number a person
    /// typed is never replaced by detection, and a missing detection changes nothing.
    ///
    /// # Errors
    ///
    /// Fails when SQLite does; then nothing changes.
    pub fn apply_identity(
        &mut self,
        phone_id: i64,
        identity: &IdentityReading,
        at: Timestamp,
    ) -> Result<Vec<NewEvent>, DatabaseError> {
        let transaction = self.connection.transaction()?;
        let at = at.to_string();
        transaction.execute(
            "UPDATE phones SET
                manufacturer = coalesce(?, manufacturer), model = coalesce(?, model),
                android_version = coalesce(?, android_version), sdk = coalesce(?, sdk),
                build_fingerprint = coalesce(?, build_fingerprint),
                imei1 = coalesce(?, imei1), imei2 = coalesce(?, imei2)
            WHERE id = ?",
            (
                &identity.manufacturer,
                &identity.model,
                &identity.android_version,
                identity.sdk,
                &identity.fingerprint,
                &identity.imei1,
                &identity.imei2,
                phone_id,
            ),
        )?;
        let mut sync = IdentitySync {
            connection: &transaction,
            at: &at,
            phone_id,
            events: Vec::new(),
        };
        for slot in &identity.slots {
            let open = sync.open_in_slot(slot.slot)?;
            match &slot.contents {
                SlotContents::Empty => {
                    if let Some(open) = open {
                        sync.close(&open, slot.slot, phone_id)?;
                    }
                }
                SlotContents::Unknown | SlotContents::Present(None) => {}
                SlotContents::Present(Some(sim)) => sync.place(slot.slot, sim, open)?,
            }
        }
        let events = sync.events;
        transaction.commit()?;
        Ok(events)
    }
}

/// One identity probe being applied.
struct IdentitySync<'transaction> {
    connection: &'transaction Connection,
    at: &'transaction str,
    phone_id: i64,
    events: Vec<NewEvent>,
}

/// An open placement.
struct OpenPlacement {
    id: i64,
    sim_id: i64,
    sub_id: i64,
    iccid: String,
}

/// An open placement anywhere.
struct ElsewherePlacement {
    placement: OpenPlacement,
    phone_id: i64,
    slot: i64,
}

impl IdentitySync<'_> {
    fn open_in_slot(&self, slot: i64) -> Result<Option<OpenPlacement>, DatabaseError> {
        let open = self
            .connection
            .query_row(
                "SELECT p.id, p.sim_id, p.sub_id, s.iccid FROM sim_placements p JOIN sims s ON s.id = p.sim_id
                WHERE p.phone_id = ? AND p.slot = ? AND p.to_at IS NULL",
                (self.phone_id, slot),
                |row| {
                    Ok(OpenPlacement {
                        id: row.get(0)?,
                        sim_id: row.get(1)?,
                        sub_id: row.get(2)?,
                        iccid: row.get(3)?,
                    })
                },
            )
            .optional()?;
        Ok(open)
    }

    /// Closes `placement`, which is in `slot` of the phone `on_phone`.
    fn close(
        &mut self,
        placement: &OpenPlacement,
        slot: i64,
        on_phone: i64,
    ) -> Result<(), DatabaseError> {
        self.connection.execute(
            "UPDATE sim_placements SET to_at = ? WHERE id = ?",
            (self.at, placement.id),
        )?;
        self.events.push(NewEvent::try_new(
            Some(on_phone),
            "sim.removed",
            &SimRemoved {
                slot,
                sim_id: placement.sim_id,
                iccid: &placement.iccid,
            },
        )?);
        Ok(())
    }

    /// Records `sim` in `slot`, where `open` is the slot's open placement.
    fn place(
        &mut self,
        slot: i64,
        sim: &SimReading,
        open: Option<OpenPlacement>,
    ) -> Result<(), DatabaseError> {
        let Some(iccid) = sim.iccid.as_deref().filter(|iccid| !iccid.is_empty()) else {
            return Ok(());
        };
        let sim_id = self.upsert_sim(iccid, sim.carrier.as_deref())?;
        match open {
            Some(open) if open.sim_id == sim_id => {
                if open.sub_id != sim.sub_id {
                    self.connection.execute(
                        "UPDATE sim_placements SET sub_id = ? WHERE id = ?",
                        (sim.sub_id, open.id),
                    )?;
                }
            }
            open => {
                if let Some(open) = open {
                    self.close(&open, slot, self.phone_id)?;
                }
                let elsewhere = self
                    .connection
                    .query_row(
                        "SELECT p.id, p.sim_id, p.phone_id, p.slot, s.iccid FROM sim_placements p JOIN sims s ON s.id = p.sim_id
                        WHERE p.sim_id = ? AND p.to_at IS NULL",
                        [sim_id],
                        |row| {
                            Ok(ElsewherePlacement {
                                placement: OpenPlacement {
                                    id: row.get(0)?,
                                    sim_id: row.get(1)?,
                                    sub_id: 0,
                                    iccid: row.get(4)?,
                                },
                                phone_id: row.get(2)?,
                                slot: row.get(3)?,
                            })
                        },
                    )
                    .optional()?;
                if let Some(elsewhere) = elsewhere {
                    self.close(&elsewhere.placement, elsewhere.slot, elsewhere.phone_id)?;
                }
                self.connection.execute(
                    "INSERT INTO sim_placements (sim_id, phone_id, slot, sub_id, from_at) VALUES (?, ?, ?, ?, ?)",
                    (sim_id, self.phone_id, slot, sim.sub_id, self.at),
                )?;
                self.events.push(NewEvent::try_new(
                    Some(self.phone_id),
                    "sim.placed",
                    &SimPlaced {
                        slot,
                        sub_id: sim.sub_id,
                        sim_id,
                        iccid,
                    },
                )?);
            }
        }
        self.bind_number(sim_id, sim.number.as_deref())
    }

    /// The SIM's id, made with a `sim.added` event on first sight. A known SIM's
    /// `last_seen_at` moves, and its carrier when the probe read one.
    fn upsert_sim(&mut self, iccid: &str, carrier: Option<&str>) -> Result<i64, DatabaseError> {
        let existing: Option<i64> = self
            .connection
            .query_row("SELECT id FROM sims WHERE iccid = ?", [iccid], |row| {
                row.get(0)
            })
            .optional()?;
        if let Some(id) = existing {
            self.connection.execute(
                "UPDATE sims SET last_seen_at = ?, carrier = coalesce(?, carrier) WHERE id = ?",
                (self.at, carrier, id),
            )?;
            return Ok(id);
        }
        let id: i64 = self.connection.query_row(
            "INSERT INTO sims (iccid, carrier, first_seen_at, last_seen_at) VALUES (?, ?, ?, ?) RETURNING id",
            (iccid, carrier, self.at, self.at),
            |row| row.get(0),
        )?;
        self.events.push(NewEvent::try_new(
            Some(self.phone_id),
            "sim.added",
            &SimAdded {
                sim_id: id,
                iccid,
                carrier,
            },
        )?);
        Ok(id)
    }

    /// Keeps the SIM's open number binding in line with the number it reports.
    fn bind_number(&mut self, sim_id: i64, detected: Option<&str>) -> Result<(), DatabaseError> {
        let binding = self
            .connection
            .query_row(
                "SELECT id, number_id, detected_e164 FROM number_bindings WHERE sim_id = ? AND to_at IS NULL",
                [sim_id],
                |row| {
                    Ok(OpenBinding {
                        id: row.get(0)?,
                        number_id: row.get(1)?,
                        detected_e164: row.get(2)?,
                    })
                },
            )
            .optional()?;
        let Some(binding) = binding else {
            let number_id = match detected.filter(|detected| !detected.is_empty()) {
                Some(detected) => self.number_for(detected)?,
                None => self.connection.query_row(
                    "INSERT INTO numbers (e164_source, created_at) VALUES ('none', ?) RETURNING id",
                    [self.at],
                    |row| row.get(0),
                )?,
            };
            self.open_binding(number_id, sim_id, detected)?;
            self.events.push(NewEvent::try_new(
                Some(self.phone_id),
                "number.bound",
                &NumberEvent {
                    sim_id,
                    number_id,
                    e164: detected,
                },
            )?);
            return Ok(());
        };
        let Some(detected) = detected else {
            return Ok(());
        };
        if binding.detected_e164.as_deref() == Some(detected) {
            return Ok(());
        }
        self.connection.execute(
            "UPDATE number_bindings SET detected_e164 = ? WHERE id = ?",
            (detected, binding.id),
        )?;
        let current = self.number_row(binding.number_id)?;
        let Some(current) = current else {
            return Ok(());
        };
        if current.source == NumberSource::Manual || current.e164.as_deref() == Some(detected) {
            return Ok(());
        }
        if current.e164.is_none() && self.number_with(detected)?.is_none() {
            self.connection.execute(
                "UPDATE numbers SET e164 = ?, e164_source = 'detected' WHERE id = ?",
                (detected, current.id),
            )?;
            self.events.push(NewEvent::try_new(
                Some(self.phone_id),
                "number.detected",
                &NumberEvent {
                    sim_id,
                    number_id: current.id,
                    e164: Some(detected),
                },
            )?);
            return Ok(());
        }
        let number_id = self.number_for(detected)?;
        self.connection.execute(
            "UPDATE number_bindings SET to_at = ? WHERE id = ?",
            (self.at, binding.id),
        )?;
        self.open_binding(number_id, sim_id, Some(detected))?;
        self.events.push(NewEvent::try_new(
            Some(self.phone_id),
            "number.changed",
            &NumberChanged {
                sim_id,
                from: current.e164.as_deref(),
                to: detected,
                number_id,
            },
        )?);
        Ok(())
    }

    /// Opens a binding of the number to the SIM, closing the number's other open binding.
    fn open_binding(
        &self,
        number_id: i64,
        sim_id: i64,
        detected: Option<&str>,
    ) -> Result<(), DatabaseError> {
        self.connection.execute(
            "UPDATE number_bindings SET to_at = ? WHERE number_id = ? AND to_at IS NULL",
            (self.at, number_id),
        )?;
        self.connection.execute(
            "INSERT INTO number_bindings (number_id, sim_id, from_at, detected_e164) VALUES (?, ?, ?, ?)",
            (number_id, sim_id, self.at, detected),
        )?;
        Ok(())
    }

    /// The id of the number with `e164`, made as detected when there is none.
    fn number_for(&self, e164: &str) -> Result<i64, DatabaseError> {
        if let Some(found) = self.number_with(e164)? {
            return Ok(found.id);
        }
        let id = self.connection.query_row(
            "INSERT INTO numbers (e164, e164_source, created_at) VALUES (?, 'detected', ?) RETURNING id",
            (e164, self.at),
            |row| row.get(0),
        )?;
        Ok(id)
    }

    fn number_with(&self, e164: &str) -> Result<Option<NumberRow>, DatabaseError> {
        let row = self
            .connection
            .query_row(
                "SELECT id, e164, e164_source FROM numbers WHERE e164 = ?",
                [e164],
                number_row_from,
            )
            .optional()?;
        Ok(row)
    }

    fn number_row(&self, id: i64) -> Result<Option<NumberRow>, DatabaseError> {
        let row = self
            .connection
            .query_row(
                "SELECT id, e164, e164_source FROM numbers WHERE id = ?",
                [id],
                number_row_from,
            )
            .optional()?;
        Ok(row)
    }
}

struct ExistingPhone {
    id: i64,
    last_state: String,
}

struct OpenBinding {
    id: i64,
    number_id: i64,
    detected_e164: Option<String>,
}

struct NumberRow {
    id: i64,
    e164: Option<String>,
    source: NumberSource,
}

fn number_row_from(row: &Row<'_>) -> rusqlite::Result<NumberRow> {
    Ok(NumberRow {
        id: row.get(0)?,
        e164: row.get(1)?,
        source: row.get(2)?,
    })
}

/// `phone.added`: `{"serial", "state"}`.
#[derive(Serialize)]
struct PhoneAdded<'detail> {
    serial: &'detail str,
    state: &'detail str,
}

/// The status events: `{"state", "rawState"?}`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StateChanged<'detail> {
    state: &'detail str,
    #[serde(skip_serializing_if = "Option::is_none")]
    raw_state: Option<&'detail str>,
}

/// `sim.added`: `{"simId", "iccid", "carrier"}`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SimAdded<'detail> {
    sim_id: i64,
    iccid: &'detail str,
    carrier: Option<&'detail str>,
}

/// `sim.placed`: `{"slot", "subId", "simId", "iccid"}`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SimPlaced<'detail> {
    slot: i64,
    sub_id: i64,
    sim_id: i64,
    iccid: &'detail str,
}

/// `sim.removed`: `{"slot", "simId", "iccid"}`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SimRemoved<'detail> {
    slot: i64,
    sim_id: i64,
    iccid: &'detail str,
}

/// `number.bound` and `number.detected`: `{"simId", "numberId", "e164"}`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NumberEvent<'detail> {
    sim_id: i64,
    number_id: i64,
    e164: Option<&'detail str>,
}

/// `number.changed`: `{"simId", "from", "to", "numberId"}`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NumberChanged<'detail> {
    sim_id: i64,
    from: Option<&'detail str>,
    to: &'detail str,
    number_id: i64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::Database;
    use crate::field_patch::FieldPatch;
    use crate::identity_reading::SlotReading;
    use crate::number_record::{NumberPatch, NumberPhone};
    use crate::number_summary::NumberSummary;
    use crate::phone_record::{PhonePatch, PlacementRecord};
    use crate::phone_state::PhoneState;
    use crate::sim_record::{LastPlacement, SimPatch, SimStatus};
    use crate::sim_summary::SimSummary;
    use crate::test_clock;

    const B30: &str = "SPARELINE00001";
    const T0: &str = "2026-10-21T10:00:00.000Z";
    const T1: &str = "2026-10-21T11:00:00.000Z";
    const T2: &str = "2026-10-21T12:00:00.000Z";

    fn sim_reading(sub_id: i64, iccid: &str, number: Option<&str>) -> SimReading {
        SimReading {
            sub_id,
            iccid: Some(iccid.to_owned()),
            carrier: Some("T-Mobile".to_owned()),
            number: number.map(str::to_owned),
        }
    }

    /// What spareline-old's probe read from the `nuu-s6702x` identity fixture.
    fn identity() -> IdentityReading {
        IdentityReading {
            manufacturer: Some("NUU".to_owned()),
            model: Some("S6702X".to_owned()),
            android_version: Some("14".to_owned()),
            sdk: Some(34),
            fingerprint: Some(
                "NUU/S6702X1/S6702XA:14/UP1A.231005.007/1726207303:user/release-keys".to_owned(),
            ),
            imei1: Some("350000000000006".to_owned()),
            imei2: Some("350000000000014".to_owned()),
            slots: vec![
                SlotReading {
                    slot: 0,
                    contents: SlotContents::Present(Some(sim_reading(
                        1,
                        "8901555000000000001",
                        Some("+15125550101"),
                    ))),
                },
                SlotReading {
                    slot: 1,
                    contents: SlotContents::Empty,
                },
            ],
        }
    }

    fn with_sim(slot: i64, sim: SimReading) -> IdentityReading {
        IdentityReading {
            slots: vec![SlotReading {
                slot,
                contents: SlotContents::Present(Some(sim)),
            }],
            ..identity()
        }
    }

    fn state(database: &mut Database, serial: &str, state: PhoneState, at: &str) -> RecordedState {
        database
            .inventory()
            .record_phone_state(&PhoneStateUpdate {
                serial: serial.to_owned(),
                state,
                site: "home".to_owned(),
                at: test_clock::at(at),
                raw_state: None,
            })
            .unwrap()
    }

    fn online(database: &mut Database, serial: &str, at: &str) -> i64 {
        state(database, serial, PhoneState::Device, at).phone_id
    }

    fn apply(
        database: &mut Database,
        phone_id: i64,
        identity: &IdentityReading,
        at: &str,
    ) -> Vec<NewEvent> {
        database
            .inventory()
            .apply_identity(phone_id, identity, test_clock::at(at))
            .unwrap()
    }

    fn kinds(events: &[NewEvent]) -> Vec<&str> {
        events.iter().map(|event| event.kind.as_str()).collect()
    }

    fn details(events: &[NewEvent]) -> Vec<&str> {
        events.iter().map(|event| event.detail.get()).collect()
    }

    fn new_database() -> Database {
        Database::open_in_memory(test_clock::fixed).unwrap()
    }

    #[test]
    fn a_numbers_count_and_last_text_come_from_its_texts() {
        use crate::message::{NewMessage, Provider};

        let mut database = new_database();
        let phone = online(&mut database, B30, T0);
        apply(&mut database, phone, &identity(), T0);
        let long_body = "x".repeat(200);
        let texts: Vec<NewMessage> = [(1, T1, "short"), (2, T2, long_body.as_str())]
            .into_iter()
            .map(|(provider_id, at, body)| NewMessage {
                provider: Provider::Sms,
                provider_id,
                sub_id: Some(1),
                sender: Some("550".to_owned()),
                body: body.to_owned(),
                subject: None,
                sent_at: None,
                received_at: test_clock::at(at),
                attachments: Vec::new(),
            })
            .collect();
        let stored = database
            .messages()
            .insert(phone, &texts, test_clock::at(T2))
            .unwrap();
        let numbers = database.inventory().list_numbers().unwrap();
        assert_eq!(numbers.len(), 1, "{numbers:?}");
        assert_eq!(numbers[0].message_count, 2);
        let last = numbers[0].last_message.as_ref().expect("a last text");
        assert_eq!(last.seq, stored[1].seq);
        assert_eq!(
            last.preview,
            "x".repeat(160),
            "the preview is the first 160 characters"
        );
        assert_eq!(last.received_at, T2);
    }

    #[test]
    fn phones_are_listed_by_serial() {
        let mut database = new_database();
        online(&mut database, "SPARELINE00002", T0);
        online(&mut database, B30, T1);
        let serials: Vec<String> = database
            .inventory()
            .list_phones()
            .unwrap()
            .into_iter()
            .map(|phone| phone.serial)
            .collect();
        assert_eq!(serials, ["SPARELINE00001", "SPARELINE00002"]);
    }

    #[test]
    fn phone_states_first_sight_needs_auth_online_transient_offline() {
        let mut database = new_database();
        let first = database
            .inventory()
            .record_phone_state(&PhoneStateUpdate {
                serial: B30.to_owned(),
                state: PhoneState::Unauthorized,
                site: "home".to_owned(),
                at: test_clock::at(T0),
                raw_state: Some("unauthorized".to_owned()),
            })
            .unwrap();
        assert_eq!(kinds(&first.events), ["phone.added", "phone.needs_auth"]);
        assert_eq!(
            details(&first.events),
            [
                r#"{"serial":"SPARELINE00001","state":"unauthorized"}"#,
                r#"{"state":"unauthorized","rawState":"unauthorized"}"#,
            ]
        );
        let back = state(&mut database, B30, PhoneState::Device, T1);
        assert_eq!(kinds(&back.events), ["phone.online"]);
        assert_eq!(details(&back.events), [r#"{"state":"device"}"#]);
        assert_eq!(
            kinds(&state(&mut database, B30, PhoneState::Offline, T2).events),
            ["phone.offline"]
        );
        assert_eq!(
            kinds(&state(&mut database, B30, PhoneState::Authorizing, T2).events),
            Vec::<&str>::new()
        );
        assert_eq!(
            database.inventory().phone(B30).unwrap().unwrap().last_state,
            "offline",
            "a transient state isn't stored"
        );
        assert_eq!(
            kinds(&state(&mut database, B30, PhoneState::Gone, T2).events),
            Vec::<&str>::new()
        );
        assert_eq!(
            kinds(&state(&mut database, B30, PhoneState::NoPermissions, T2).events),
            ["phone.no_permissions"]
        );
        assert_eq!(
            kinds(&state(&mut database, B30, PhoneState::Recovery, T2).events),
            ["phone.other_state"]
        );
        let phone = database.inventory().phone(B30).unwrap().unwrap();
        assert_eq!(phone.last_state, "recovery");
        assert_eq!(phone.first_seen_at, T0);
        assert_eq!(phone.last_online_at.as_deref(), Some(T1));
        assert_eq!(phone.site, "home");
        assert_eq!(database.inventory().serials_not_gone().unwrap(), [B30]);
        state(&mut database, B30, PhoneState::Gone, T2);
        assert_eq!(
            database.inventory().serials_not_gone().unwrap(),
            Vec::<String>::new()
        );
        assert_eq!(
            database.inventory().phone_id(B30).unwrap(),
            Some(first.phone_id)
        );
        assert_eq!(database.inventory().phone_id("other").unwrap(), None);
    }

    #[test]
    fn first_identity_records_the_phone_its_sim_placement_and_number() {
        let mut database = new_database();
        let phone_id = online(&mut database, B30, T0);
        let events = apply(&mut database, phone_id, &identity(), T0);
        assert_eq!(kinds(&events), ["sim.added", "sim.placed", "number.bound"]);
        assert_eq!(
            details(&events),
            [
                r#"{"simId":1,"iccid":"8901555000000000001","carrier":"T-Mobile"}"#,
                r#"{"slot":0,"subId":1,"simId":1,"iccid":"8901555000000000001"}"#,
                r#"{"simId":1,"numberId":1,"e164":"+15125550101"}"#,
            ]
        );
        let phone = database.inventory().phone(B30).unwrap().unwrap();
        assert_eq!(phone.model.as_deref(), Some("S6702X"));
        assert_eq!(phone.manufacturer.as_deref(), Some("NUU"));
        assert_eq!(phone.android_version.as_deref(), Some("14"));
        assert_eq!(phone.sdk, Some(34));
        assert_eq!(phone.imeis, ["350000000000006", "350000000000014"]);
        assert_eq!(
            phone.placements,
            [PlacementRecord {
                slot: 0,
                sub_id: 1,
                since: T0.to_owned(),
                sim: SimSummary {
                    id: 1,
                    iccid: "8901555000000000001".to_owned(),
                    carrier: Some("T-Mobile".to_owned()),
                },
                number: Some(NumberSummary {
                    id: 1,
                    e164: Some("+15125550101".to_owned()),
                    source: NumberSource::Detected,
                    note: String::new(),
                }),
            }]
        );
        assert!(
            apply(&mut database, phone_id, &identity(), T1).is_empty(),
            "probing again changes nothing"
        );
        let again = database.inventory().phone(B30).unwrap().unwrap();
        assert_eq!(again.placements[0].since, T0);
        let fingerprint: String = database
            .connection
            .query_row("SELECT build_fingerprint FROM phones", [], |row| row.get(0))
            .unwrap();
        assert!(fingerprint.starts_with("NUU/"), "{fingerprint}");
    }

    #[test]
    fn facts_the_probe_missed_keep_their_values() {
        let mut database = new_database();
        let phone_id = online(&mut database, B30, T0);
        apply(&mut database, phone_id, &identity(), T0);
        let blank = IdentityReading {
            slots: Vec::new(),
            ..IdentityReading::default()
        };
        apply(&mut database, phone_id, &blank, T1);
        let phone = database.inventory().phone(B30).unwrap().unwrap();
        assert_eq!(phone.model.as_deref(), Some("S6702X"));
        assert_eq!(phone.imeis.len(), 2);
        assert_eq!(phone.placements.len(), 1);
    }

    #[test]
    fn a_removed_sim_closes_its_placement_and_the_number_remembers_where_it_was() {
        let mut database = new_database();
        let phone_id = online(&mut database, B30, T0);
        apply(&mut database, phone_id, &identity(), T0);
        let removed = IdentityReading {
            slots: vec![
                SlotReading {
                    slot: 0,
                    contents: SlotContents::Empty,
                },
                SlotReading {
                    slot: 1,
                    contents: SlotContents::Empty,
                },
            ],
            ..identity()
        };
        let events = apply(&mut database, phone_id, &removed, T1);
        assert_eq!(kinds(&events), ["sim.removed"]);
        assert_eq!(
            details(&events),
            [r#"{"slot":0,"simId":1,"iccid":"8901555000000000001"}"#]
        );
        assert_eq!(
            database.inventory().phone(B30).unwrap().unwrap().placements,
            []
        );
        assert_eq!(
            database
                .inventory()
                .find_number("+15125550101")
                .unwrap()
                .unwrap()
                .phone,
            Some(NumberPhone {
                serial: B30.to_owned(),
                model: Some("S6702X".to_owned()),
                slot: 0,
                removed_at: Some(T1.to_owned()),
            })
        );
        let sim = database.inventory().sim(1).unwrap().unwrap();
        assert_eq!(sim.placement, None);
        assert_eq!(
            sim.last_placement,
            Some(LastPlacement {
                serial: B30.to_owned(),
                slot: 0,
                until: T1.to_owned(),
            })
        );
    }

    #[test]
    fn a_slot_that_is_still_loading_keeps_its_placement() {
        let mut database = new_database();
        let phone_id = online(&mut database, B30, T0);
        apply(&mut database, phone_id, &identity(), T0);
        let loading = IdentityReading {
            slots: vec![SlotReading {
                slot: 0,
                contents: SlotContents::Unknown,
            }],
            ..identity()
        };
        assert!(apply(&mut database, phone_id, &loading, T1).is_empty());
        let without_iccid = with_sim(
            0,
            SimReading {
                iccid: None,
                ..sim_reading(1, "", None)
            },
        );
        assert!(apply(&mut database, phone_id, &without_iccid, T1).is_empty());
        assert_eq!(
            database
                .inventory()
                .phone(B30)
                .unwrap()
                .unwrap()
                .placements
                .len(),
            1
        );
    }

    #[test]
    fn a_sim_moved_to_another_phone_takes_its_number_along() {
        let mut database = new_database();
        let a = online(&mut database, B30, T0);
        apply(&mut database, a, &identity(), T0);
        let b = online(&mut database, "OTHERPHONE", T1);
        let moved = IdentityReading {
            slots: vec![
                SlotReading {
                    slot: 0,
                    contents: SlotContents::Empty,
                },
                SlotReading {
                    slot: 1,
                    contents: SlotContents::Present(Some(sim_reading(
                        2,
                        "8901555000000000001",
                        Some("+15125550101"),
                    ))),
                },
            ],
            ..identity()
        };
        let events = apply(&mut database, b, &moved, T1);
        let placed: Vec<(&str, Option<i64>)> = events
            .iter()
            .map(|event| (event.kind.as_str(), event.phone_id))
            .collect();
        assert_eq!(placed, [("sim.removed", Some(a)), ("sim.placed", Some(b))]);
        assert_eq!(
            database.inventory().phone(B30).unwrap().unwrap().placements,
            []
        );
        let number = database
            .inventory()
            .find_number("+15125550101")
            .unwrap()
            .unwrap();
        assert_eq!(number.id, 1);
        let phone = number.phone.unwrap();
        assert_eq!(
            (phone.serial.as_str(), phone.slot, phone.removed_at),
            ("OTHERPHONE", 1, None)
        );
        let sim = database.inventory().sim(1).unwrap().unwrap();
        assert_eq!(sim.placement.unwrap().sub_id, 2);
    }

    #[test]
    fn a_changed_sub_id_updates_the_placement_in_place() {
        let mut database = new_database();
        let phone_id = online(&mut database, B30, T0);
        apply(&mut database, phone_id, &identity(), T0);
        let renumbered = with_sim(
            0,
            sim_reading(5, "8901555000000000001", Some("+15125550101")),
        );
        assert!(apply(&mut database, phone_id, &renumbered, T1).is_empty());
        let placement = &database.inventory().phone(B30).unwrap().unwrap().placements[0];
        assert_eq!((placement.sub_id, placement.since.as_str()), (5, T0));
    }

    #[test]
    fn a_number_typed_by_the_owner_survives_detection_and_can_be_cleared() {
        let mut database = new_database();
        let phone_id = online(&mut database, B30, T0);
        apply(&mut database, phone_id, &identity(), T0);
        let now = test_clock::START;
        database
            .inventory()
            .patch_number(
                1,
                &NumberPatch {
                    note: None,
                    e164: FieldPatch::Value("+15125550199".to_owned()),
                },
                Some("owner@example.com"),
                now,
            )
            .unwrap();
        let number = database.inventory().number(1).unwrap().unwrap();
        assert_eq!(number.e164.as_deref(), Some("+15125550199"));
        assert_eq!(number.source, NumberSource::Manual);
        assert_eq!(number.detected_e164.as_deref(), Some("+15125550101"));
        assert_eq!(number.confirmed_by.as_deref(), Some("owner@example.com"));
        assert!(apply(&mut database, phone_id, &identity(), T1).is_empty());
        assert_eq!(
            database
                .inventory()
                .number(1)
                .unwrap()
                .unwrap()
                .e164
                .as_deref(),
            Some("+15125550199")
        );
        database
            .inventory()
            .patch_number(
                1,
                &NumberPatch {
                    note: None,
                    e164: FieldPatch::Null,
                },
                None,
                now,
            )
            .unwrap();
        let cleared = database.inventory().number(1).unwrap().unwrap();
        assert_eq!(cleared.e164.as_deref(), Some("+15125550101"));
        assert_eq!(cleared.source, NumberSource::Detected);
        assert_eq!(cleared.confirmed_by, None);
        let mut statement = database
            .connection
            .prepare("SELECT at, coalesce(login, 'null'), entity, entity_id, changes_json FROM inventory_edits ORDER BY id")
            .unwrap();
        let audit: Vec<String> = statement
            .query_map([], |row| {
                Ok(format!(
                    "{} {} {} {} {}",
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?
                ))
            })
            .unwrap()
            .collect::<Result<Vec<String>, _>>()
            .unwrap();
        assert_eq!(
            audit,
            [
                r#"2026-10-26T00:00:00.000Z owner@example.com number 1 {"e164":"+15125550199"}"#,
                r#"2026-10-26T00:00:00.000Z null number 1 {"e164":null}"#,
            ]
        );
    }

    #[test]
    fn a_patch_without_login_confirms_as_unknown_and_clearing_without_detection_leaves_none() {
        let mut database = new_database();
        let phone_id = online(&mut database, B30, T0);
        apply(
            &mut database,
            phone_id,
            &with_sim(0, sim_reading(1, "8901555000000000001", None)),
            T0,
        );
        let now = test_clock::START;
        let set = database
            .inventory()
            .patch_number(
                1,
                &NumberPatch {
                    note: None,
                    e164: FieldPatch::Value("+15125550123".to_owned()),
                },
                None,
                now,
            )
            .unwrap();
        assert_eq!(set.confirmed_by.as_deref(), Some("unknown"));
        let cleared = database
            .inventory()
            .patch_number(
                1,
                &NumberPatch {
                    note: None,
                    e164: FieldPatch::Null,
                },
                None,
                now,
            )
            .unwrap();
        assert_eq!((cleared.e164, cleared.source), (None, NumberSource::None));
        let missing = database
            .inventory()
            .patch_number(9, &NumberPatch::default(), None, now)
            .unwrap_err();
        assert_eq!(missing.to_string(), "no number 9");
    }

    #[test]
    fn a_sim_without_a_detected_number_gets_an_empty_number_that_detection_fills_in() {
        let mut database = new_database();
        let phone_id = online(&mut database, B30, T0);
        let silent = with_sim(0, sim_reading(1, "8901555000000000001", None));
        let events = apply(&mut database, phone_id, &silent, T0);
        assert_eq!(
            details(&events)[2],
            r#"{"simId":1,"numberId":1,"e164":null}"#
        );
        let number = database.inventory().number(1).unwrap().unwrap();
        assert_eq!((number.e164, number.source), (None, NumberSource::None));
        let events = apply(&mut database, phone_id, &identity(), T1);
        assert_eq!(kinds(&events), ["number.detected"]);
        assert_eq!(
            details(&events),
            [r#"{"simId":1,"numberId":1,"e164":"+15125550101"}"#]
        );
        let number = database.inventory().number(1).unwrap().unwrap();
        assert_eq!(number.e164.as_deref(), Some("+15125550101"));
        assert_eq!(number.source, NumberSource::Detected);
        assert_eq!(number.detected_e164.as_deref(), Some("+15125550101"));
        assert!(
            apply(&mut database, phone_id, &silent, T2).is_empty(),
            "a missing detection changes nothing"
        );
    }

    #[test]
    fn a_sim_that_reports_a_different_number_moves_to_a_new_number_row() {
        let mut database = new_database();
        let phone_id = online(&mut database, B30, T0);
        apply(&mut database, phone_id, &identity(), T0);
        let changed = with_sim(
            0,
            sim_reading(1, "8901555000000000001", Some("+15125550102")),
        );
        let events = apply(&mut database, phone_id, &changed, T1);
        assert_eq!(kinds(&events), ["number.changed"]);
        assert_eq!(
            details(&events),
            [r#"{"simId":1,"from":"+15125550101","to":"+15125550102","numberId":2}"#]
        );
        let new = database
            .inventory()
            .find_number("+15125550102")
            .unwrap()
            .unwrap();
        assert_eq!(new.phone.unwrap().serial, B30);
        let old = database
            .inventory()
            .find_number("+15125550101")
            .unwrap()
            .unwrap();
        assert_eq!(old.sim.unwrap().iccid, "8901555000000000001");
        let phone = database.inventory().phone(B30).unwrap().unwrap();
        assert_eq!(
            phone.placements[0].number.as_ref().unwrap().e164.as_deref(),
            Some("+15125550102")
        );
    }

    #[test]
    fn a_numbers_note_survives_probes_and_conflicts_are_refused() {
        let mut database = new_database();
        let phone_id = online(&mut database, B30, T0);
        apply(&mut database, phone_id, &identity(), T0);
        let now = test_clock::START;
        let noted = database
            .inventory()
            .patch_number(
                1,
                &NumberPatch {
                    note: Some("Craigslist couch".to_owned()),
                    e164: FieldPatch::Absent,
                },
                None,
                now,
            )
            .unwrap();
        assert_eq!(noted.note, "Craigslist couch");
        apply(&mut database, phone_id, &identity(), T1);
        let phone = database.inventory().phone(B30).unwrap().unwrap();
        assert_eq!(
            phone.placements[0].number.as_ref().unwrap().note,
            "Craigslist couch"
        );
        let second = online(&mut database, "SECOND", T0);
        apply(
            &mut database,
            second,
            &with_sim(0, sim_reading(1, "8901555000000000002", None)),
            T0,
        );
        let error = database
            .inventory()
            .patch_number(
                2,
                &NumberPatch {
                    note: Some("lost".to_owned()),
                    e164: FieldPatch::Value("+15125550101".to_owned()),
                },
                None,
                now,
            )
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "+15125550101 already belongs to number 1"
        );
        assert_eq!(
            database.inventory().number(2).unwrap().unwrap().note,
            "",
            "a refused patch changes nothing"
        );
        let edits: i64 = database
            .connection
            .query_row("SELECT count(*) FROM inventory_edits", [], |row| row.get(0))
            .unwrap();
        assert_eq!(edits, 1);
    }

    #[test]
    fn phone_and_sim_patches_write_their_fields_and_an_audit_row() {
        let mut database = new_database();
        let phone_id = online(&mut database, B30, T0);
        apply(&mut database, phone_id, &identity(), T0);
        let now = test_clock::START;
        let patched = database
            .inventory()
            .patch_phone(
                B30,
                &PhonePatch {
                    location: FieldPatch::Value("shelf 2".to_owned()),
                    cost_cents: FieldPatch::Value(1999),
                    acquired_on: FieldPatch::Null,
                    ..PhonePatch::default()
                },
                Some("alice"),
                now,
            )
            .unwrap();
        assert_eq!(patched.location.as_deref(), Some("shelf 2"));
        assert_eq!(patched.cost_cents, Some(1999));
        assert_eq!(patched.hub_port, None);
        database
            .inventory()
            .patch_phone(
                B30,
                &PhonePatch {
                    cost_cents: FieldPatch::Null,
                    ..PhonePatch::default()
                },
                None,
                now,
            )
            .unwrap();
        let phone = database.inventory().phone(B30).unwrap().unwrap();
        assert_eq!(
            (phone.location.as_deref(), phone.cost_cents),
            (Some("shelf 2"), None)
        );
        database
            .inventory()
            .patch_phone(B30, &PhonePatch::default(), None, now)
            .unwrap();
        let missing = database
            .inventory()
            .patch_phone("NOPE", &PhonePatch::default(), None, now)
            .unwrap_err();
        assert_eq!(missing.to_string(), "no phone NOPE");

        let sim = database
            .inventory()
            .patch_sim(
                1,
                &SimPatch {
                    status: Some(SimStatus::Spare),
                },
                Some("alice"),
                now,
            )
            .unwrap();
        assert_eq!(sim.status, SimStatus::Spare);
        assert_eq!(
            database
                .inventory()
                .patch_sim(7, &SimPatch::default(), None, now)
                .unwrap_err()
                .to_string(),
            "no SIM 7"
        );
        let mut statement = database
            .connection
            .prepare("SELECT entity || ' ' || entity_id || ' ' || changes_json FROM inventory_edits ORDER BY id")
            .unwrap();
        let audit: Vec<String> = statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<String>, _>>()
            .unwrap();
        assert_eq!(
            audit,
            [
                r#"phone 1 {"location":"shelf 2","costCents":1999,"acquiredOn":null}"#,
                r#"phone 1 {"costCents":null}"#,
                "phone 1 {}",
                r#"sim 1 {"status":"spare"}"#,
            ]
        );
    }

    #[test]
    fn sims_are_found_by_id_or_iccid_and_serve_their_json() {
        let mut database = new_database();
        let phone_id = online(&mut database, B30, T0);
        apply(&mut database, phone_id, &identity(), T0);
        let inventory = database.inventory();
        let by_id = inventory.find_sim("1").unwrap().unwrap();
        assert_eq!(
            inventory.find_sim("8901555000000000001").unwrap(),
            Some(by_id.clone())
        );
        assert_eq!(
            inventory.find_sim("0001").unwrap().map(|sim| sim.id),
            Some(1)
        );
        assert_eq!(inventory.find_sim("2").unwrap(), None);
        assert_eq!(inventory.list_sims().unwrap(), vec![by_id.clone()]);
        assert_eq!(
            serde_json::to_string(&by_id).unwrap(),
            r#"{"id":1,"iccid":"8901555000000000001","carrier":"T-Mobile","status":"active","firstSeenAt":"2026-10-21T10:00:00.000Z","lastSeenAt":"2026-10-21T10:00:00.000Z","placement":{"phoneId":1,"serial":"SPARELINE00001","slot":0,"subId":1,"since":"2026-10-21T10:00:00.000Z"},"lastPlacement":null,"number":{"id":1,"e164":"+15125550101","source":"detected","note":""}}"#
        );
    }

    #[test]
    fn numbers_are_found_by_e164_or_id_and_listed_known_first() {
        let mut database = new_database();
        let phone_id = online(&mut database, B30, T0);
        apply(
            &mut database,
            phone_id,
            &with_sim(0, sim_reading(1, "8901555000000000002", None)),
            T0,
        );
        apply(
            &mut database,
            phone_id,
            &with_sim(
                1,
                sim_reading(2, "8901555000000000001", Some("+15125550101")),
            ),
            T0,
        );
        let inventory = database.inventory();
        assert_eq!(inventory.number_id_of("+15125550101").unwrap(), Some(2));
        assert_eq!(inventory.number_id_of("1").unwrap(), Some(1));
        assert_eq!(
            inventory.number_id_of("5125550101").unwrap(),
            None,
            "bare digits aren't a key here"
        );
        assert_eq!(inventory.number_id_of("1234567890").unwrap(), None);
        assert_eq!(inventory.number_id_of("+1999").unwrap(), None);
        let ids: Vec<i64> = inventory
            .list_numbers()
            .unwrap()
            .iter()
            .map(|number| number.id)
            .collect();
        assert_eq!(ids, [2, 1]);
        let number = inventory.number(2).unwrap().unwrap();
        assert_eq!(
            serde_json::to_string(&number).unwrap(),
            r#"{"id":2,"e164":"+15125550101","source":"detected","note":"","detectedE164":"+15125550101","confirmedBy":null,"createdAt":"2026-10-21T10:00:00.000Z","sim":{"id":2,"iccid":"8901555000000000001","carrier":"T-Mobile"},"phone":{"serial":"SPARELINE00001","model":"S6702X","slot":1,"removedAt":null},"messageCount":0,"lastMessage":null}"#
        );
        assert_eq!(number.summary().id, 2);
    }
}
