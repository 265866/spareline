//! What the phone manager's tasks share, and the state changes they make together: adb
//! reachability, phone states and sessions, each written to the database before it is
//! published.

use std::collections::HashSet;
use std::time::Duration;

use spareline_adb::{DeviceEntry, DeviceState};
use spareline_core::PhoneStatus;
use spareline_db::{DatabaseError, NewEvent, PhoneState, PhoneStateUpdate};
use tokio::sync::mpsc;

use crate::buses::Buses;
use crate::db_handle::DbHandle;
use crate::devices::handle::Devices;
use crate::devices::phone_event::PhoneEvent;
use crate::devices::probe_reason::ProbeReason;
use crate::devices::probe_worker;
use crate::event_log::EventLog;

/// The phone manager's shared parts. Cloning is cheap: each part is a handle.
#[derive(Clone, Debug)]
pub(crate) struct ManagerContext {
    /// The phone table and the calls to phones.
    pub(crate) devices: Devices,
    /// The database.
    pub(crate) db: DbHandle,
    /// Where recorded device events are published.
    pub(crate) buses: Buses,
    /// The site a new phone row gets.
    pub(crate) site: String,
    /// The log.
    pub(crate) log: EventLog,
    /// The delays before each retry of a phone's first identity probe.
    pub(crate) identity_retry: Vec<Duration>,
}

impl ManagerContext {
    /// Records `events` at the current time and publishes them once committed, while the
    /// database is still held, so events reach the bus in the order of their ids whichever task
    /// writes them: a stream drops an id at or below its cursor, and would lose one published
    /// late.
    ///
    /// # Errors
    ///
    /// Fails when SQLite does; then nothing is published.
    pub(crate) async fn publish(&self, events: Vec<NewEvent>) -> Result<(), DatabaseError> {
        if events.is_empty() {
            return Ok(());
        }
        let now = self.devices.clock().now();
        let buses = self.buses.clone();
        self.db
            .call(move |db| {
                let recorded = db.device_events().insert(&events, now)?;
                buses.publish_device_events(&recorded);
                Ok(())
            })
            .await
    }

    /// Records an attempt to reach the adb server: a change that is news is recorded in the
    /// database first, then the table shows it and it is logged. Only the track task calls this,
    /// so the table can't change between the check and the update.
    ///
    /// # Errors
    ///
    /// Fails when the event can't be recorded; then the table is left as it was, so the next
    /// attempt finds the change still news and records it.
    pub(crate) async fn set_adb(
        &self,
        reachable: bool,
        version: Option<u32>,
        error: Option<String>,
    ) -> Result<(), DatabaseError> {
        let news = self.devices.table().adb_news(reachable);
        if news {
            let recorded = if reachable {
                NewEvent::adb_reachable()?
            } else {
                NewEvent::adb_unreachable(error.as_deref().unwrap_or(""))?
            };
            self.publish(vec![recorded]).await?;
        }
        let now = self.devices.clock().now();
        self.devices
            .table()
            .set_adb(reachable, version, error.clone(), now);
        if news {
            let event = if reachable {
                "adb-reachable"
            } else {
                "adb-unreachable"
            };
            self.log
                .event(event)
                .field("version", &version)
                .field("error", &error)
                .write();
        }
        Ok(())
    }

    /// Applies one device list from the adb server: each listed phone's state, then `gone` for
    /// every phone it no longer lists.
    ///
    /// # Errors
    ///
    /// Fails when a state can't be recorded; the phones after it are left for the next list.
    pub(crate) async fn apply_list(&self, list: &[DeviceEntry]) -> Result<(), DatabaseError> {
        for entry in list {
            self.apply_state(&entry.serial, phone_state(entry.state), Some(entry))
                .await?;
        }
        let seen: HashSet<String> = list.iter().map(|entry| entry.serial.clone()).collect();
        self.mark_unseen_gone(&seen).await
    }

    /// Marks every phone not in `seen` as gone: the phones this run knows, and the phones the
    /// database last saw in a settled state other than offline, which after a restart would
    /// otherwise keep a stale state.
    ///
    /// # Errors
    ///
    /// Fails when the database does.
    pub(crate) async fn mark_unseen_gone(
        &self,
        seen: &HashSet<String>,
    ) -> Result<(), DatabaseError> {
        let known = self.devices.table().serials();
        let unseen = known.iter().filter(|serial| !seen.contains(*serial));
        for serial in unseen {
            self.apply_state(serial, PhoneState::Gone, None).await?;
        }
        let stored = self.db.call(|db| db.inventory().serials_not_gone()).await?;
        for serial in stored {
            if !seen.contains(&serial) && self.devices.get(&serial).is_none() {
                self.apply_state(&serial, PhoneState::Gone, None).await?;
            }
        }
        Ok(())
    }

    /// Applies one phone's state: the database and its events, then the table, then the log,
    /// then the session, which opens when the phone comes online and ends when it leaves. Only
    /// the track task applies states, so the table can't change between the check and the
    /// update.
    ///
    /// # Errors
    ///
    /// Fails when the state can't be recorded; then the table and the session are left as they
    /// were, so the next device list applies the state again.
    async fn apply_state(
        &self,
        serial: &str,
        state: PhoneState,
        entry: Option<&DeviceEntry>,
    ) -> Result<(), DatabaseError> {
        let now = self.devices.clock().now();
        if self
            .devices
            .get(serial)
            .is_some_and(|phone| phone.adb_state == state)
        {
            return Ok(());
        }
        let update = PhoneStateUpdate {
            serial: serial.to_owned(),
            state,
            site: self.site.clone(),
            at: now,
            raw_state: entry.map(|entry| entry.raw_state.clone()),
        };
        let buses = self.buses.clone();
        self.db
            .call(move |db| {
                let recorded = db.inventory().record_phone_state(&update)?;
                let events = db.device_events().insert(&recorded.events, now)?;
                buses.publish_device_events(&events);
                Ok::<(), DatabaseError>(())
            })
            .await?;
        let usb = entry.and_then(|entry| entry.usb.as_deref());
        let Some(change) = self.devices.table().apply_state(serial, state, usb, now) else {
            return Ok(());
        };
        self.log
            .event("phone-state")
            .field("serial", serial)
            .field("state", state.as_str())
            .write();
        if change.after == PhoneStatus::Online && !change.has_session {
            self.start_session(serial);
        } else if change.after != PhoneStatus::Online && change.has_session {
            self.devices.table().end_session(serial);
            self.devices.probe_done();
            self.devices.emit(PhoneEvent::Disconnected {
                serial: serial.to_owned(),
            });
        }
        Ok(())
    }

    /// Opens a session for a phone that came online: starts its probe worker, which waits for
    /// the previous session's, queues its connect probe, and announces it.
    fn start_session(&self, serial: &str) {
        let (probes, queue) = mpsc::unbounded_channel();
        let Some(start) = self.devices.table().start_session(serial, probes) else {
            return;
        };
        let worker = tokio::spawn(probe_worker::run(
            self.clone(),
            serial.to_owned(),
            start.token,
            queue,
            start.previous_worker,
        ));
        let mut table = self.devices.table();
        table.set_worker(serial, worker);
        table.schedule(serial, ProbeReason::Connect);
        drop(table);
        self.devices.emit(PhoneEvent::Connected {
            serial: serial.to_owned(),
        });
    }
}

/// The state a phone in adb's list is in, as the database stores it.
fn phone_state(state: DeviceState) -> PhoneState {
    match state {
        DeviceState::Device => PhoneState::Device,
        DeviceState::Unauthorized => PhoneState::Unauthorized,
        DeviceState::Offline => PhoneState::Offline,
        DeviceState::NoPermissions => PhoneState::NoPermissions,
        DeviceState::Connecting => PhoneState::Connecting,
        DeviceState::Authorizing => PhoneState::Authorizing,
        DeviceState::Bootloader => PhoneState::Bootloader,
        DeviceState::Recovery => PhoneState::Recovery,
        DeviceState::Rescue => PhoneState::Rescue,
        DeviceState::Sideload => PhoneState::Sideload,
        DeviceState::Host => PhoneState::Host,
        DeviceState::Detached => PhoneState::Detached,
        DeviceState::Unknown => PhoneState::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;
    use serde_json::Value;
    use spareline_adb::{AdbClient, Endpoint};
    use spareline_core::Timestamp;
    use spareline_core::monitor::{MonitorEvent, ProvisionReport};
    use spareline_db::Database;
    use tokio::runtime::Builder;
    use tokio::task::JoinSet;

    use super::*;
    use crate::clock::Clock;
    use crate::fake_adb::{self, ScratchDir};
    use crate::monitor::{Monitor, MonitorContext, MonitorSettings};

    #[test]
    fn concurrent_manager_and_monitor_writers_publish_device_events_in_the_order_they_committed() {
        Builder::new_multi_thread()
            .worker_threads(8)
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let buses = Buses::new();
                let mut published = buses.device_events();
                let devices = Devices::new(
                    AdbClient::new(Endpoint::new("127.0.0.1".to_owned(), 9)),
                    Clock::system(),
                );
                let db = DbHandle::new(Database::open_in_memory(Timestamp::now).unwrap());
                let context = ManagerContext {
                    devices: devices.clone(),
                    db: db.clone(),
                    buses: buses.clone(),
                    site: "home".to_owned(),
                    log: EventLog::discard(),
                    identity_retry: Vec::new(),
                };
                let monitor = Monitor::new(
                    MonitorSettings::default(),
                    MonitorContext {
                        devices,
                        db,
                        buses,
                        clock: Clock::system(),
                        log: EventLog::discard(),
                    },
                );
                let report = ProvisionReport::failed(Timestamp::now(), 0, "a stand-in run");
                let mut writers = JoinSet::new();
                for _ in 0..400 {
                    let context = context.clone();
                    writers.spawn(async move {
                        let event = NewEvent::adb_reachable().unwrap();
                        context.publish(vec![event]).await.unwrap();
                    });
                    let monitor = monitor.clone();
                    let report = report.clone();
                    writers.spawn(async move {
                        monitor
                            .publish("", &[MonitorEvent::Provision { report }])
                            .await;
                    });
                }
                while let Some(written) = writers.join_next().await {
                    written.unwrap();
                }
                let mut ids = Vec::new();
                let mut kinds = HashSet::new();
                while let Ok(event) = published.try_recv() {
                    ids.push(event.id);
                    kinds.insert(event.kind);
                }
                let mut sorted = ids.clone();
                sorted.sort_unstable();
                assert_eq!(ids.len(), 800);
                assert_eq!(kinds.len(), 2, "both writers published: {kinds:?}");
                assert_eq!(
                    ids, sorted,
                    "a stream would drop every id published after a higher one"
                );
            });
    }

    #[test]
    fn an_adb_change_the_database_refuses_leaves_the_table_as_it_was_and_records_on_the_next_try() {
        fake_adb::block_on(async {
            let dir = ScratchDir::new();
            let path = dir.path().join("spareline.db");
            let buses = Buses::new();
            let mut published = buses.device_events();
            let log = EventLog::memory(Clock::tokio(fake_adb::START));
            let context = ManagerContext {
                devices: Devices::new(
                    AdbClient::new(Endpoint::new("127.0.0.1".to_owned(), 9)),
                    Clock::tokio(fake_adb::START),
                ),
                db: DbHandle::new(Database::open(&path, Timestamp::now, |_| {}).unwrap()),
                buses,
                site: "home".to_owned(),
                log: log.clone(),
                identity_retry: Vec::new(),
            };
            context
                .set_adb(false, None, Some("refused".to_owned()))
                .await
                .unwrap();
            assert_eq!(published.try_recv().unwrap().kind, "adb.unreachable");
            let tamper = Connection::open(&path).unwrap();
            tamper
                .execute_batch("ALTER TABLE device_events RENAME TO device_events_away")
                .unwrap();
            assert!(context.set_adb(true, Some(41), None).await.is_err());
            assert!(
                !context.devices.adb().reachable,
                "the table shows only what was recorded"
            );
            tamper
                .execute_batch("ALTER TABLE device_events_away RENAME TO device_events")
                .unwrap();
            drop(tamper);
            context.set_adb(true, Some(41), None).await.unwrap();
            assert!(context.devices.adb().reachable);
            assert_eq!(
                published.try_recv().unwrap().kind,
                "adb.reachable",
                "the refused change is still news on the next try"
            );
            let logged: Vec<String> = log
                .lines()
                .iter()
                .map(|line| serde_json::from_str::<Value>(line).unwrap())
                .map(|line| line["event"].as_str().unwrap().to_owned())
                .collect();
            assert_eq!(
                logged,
                ["adb-unreachable", "adb-reachable"],
                "each change is logged once, when it is recorded"
            );
        });
    }

    #[test]
    fn device_state_maps_to_the_stored_state() {
        let states = [
            DeviceState::Device,
            DeviceState::Unauthorized,
            DeviceState::Offline,
            DeviceState::NoPermissions,
            DeviceState::Connecting,
            DeviceState::Authorizing,
            DeviceState::Bootloader,
            DeviceState::Recovery,
            DeviceState::Rescue,
            DeviceState::Sideload,
            DeviceState::Host,
            DeviceState::Detached,
            DeviceState::Unknown,
        ];
        for state in states {
            assert_eq!(phone_state(state).as_str(), state.as_str());
        }
    }
}
