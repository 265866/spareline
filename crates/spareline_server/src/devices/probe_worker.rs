//! A phone's probes: identity with its retries, and health, run one at a time by the worker of
//! the phone's session.

use spareline_core::{
    IdentityProbe, PROBE_TIMEOUT, PhoneHealth, PhoneIdentity, ProbeError, SlotPresence,
};
use spareline_db::{
    DatabaseError, DeviceEvent, IdentityReading, SimReading, SlotContents, SlotReading,
};
use thiserror::Error;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::task::JoinHandle;
use tokio::time;

use crate::devices::device_error::DeviceError;
use crate::devices::manager_context::ManagerContext;
use crate::devices::phone_event::PhoneEvent;
use crate::devices::probe_reason::ProbeReason;
use crate::devices::session::SessionToken;
use crate::shell_text;

/// A probe that failed.
#[derive(Debug, Error)]
#[must_use]
enum ProbeFailure {
    /// The phone couldn't be reached.
    #[error(transparent)]
    Device(#[from] DeviceError),
    /// The script exited with a status above 1.
    #[error(transparent)]
    Script(#[from] ProbeError),
    /// What it read couldn't be recorded.
    #[error(transparent)]
    Database(#[from] DatabaseError),
}

/// Runs the probes of one session of `serial` as they are queued, one at a time, after the
/// previous session's worker finished, so two probes of a phone never overlap. It stops once the
/// session ended and its queue is drained; a probe queued after the end is skipped.
pub(crate) async fn run(
    context: ManagerContext,
    serial: String,
    token: SessionToken,
    mut probes: UnboundedReceiver<ProbeReason>,
    previous: Option<JoinHandle<()>>,
) {
    if let Some(previous) = previous {
        match previous.await {
            Ok(()) | Err(_) => {}
        }
    }
    while let Some(reason) = probes.recv().await {
        context
            .devices
            .table()
            .probe_started(&serial, token.id(), reason);
        if !token.is_ended()
            && let Err(failure) = probe(&context, &serial, &token, reason).await
            && !token.is_ended()
        {
            let error = failure.to_string();
            context
                .devices
                .table()
                .set_probe_error(&serial, token.id(), error.clone());
            context
                .log
                .event("probe-failed")
                .field("serial", &serial)
                .field("reason", reason.as_str())
                .field("error", &error)
                .write();
        }
        context.devices.table().probe_finished(&serial, token.id());
        context.devices.probe_done();
    }
}

async fn probe(
    context: &ManagerContext,
    serial: &str,
    token: &SessionToken,
    reason: ProbeReason,
) -> Result<(), ProbeFailure> {
    match reason {
        ProbeReason::Identity => read_identity(context, serial, token).await,
        ProbeReason::Connect => {
            let identity = first_identity(context, serial, token).await;
            if !token.is_ended() {
                context.devices.emit(PhoneEvent::Ready {
                    serial: serial.to_owned(),
                });
            }
            identity?;
            read_health(context, serial, token, reason).await
        }
        ProbeReason::Health => read_health(context, serial, token, reason).await,
    }
}

/// The first identity read of a session, retried after each delay while the phone's telephony
/// stack may still be starting.
async fn first_identity(
    context: &ManagerContext,
    serial: &str,
    token: &SessionToken,
) -> Result<(), ProbeFailure> {
    let mut retries = context.identity_retry.iter();
    loop {
        let Err(failure) = read_identity(context, serial, token).await else {
            return Ok(());
        };
        let Some(&delay) = retries.next() else {
            return Err(failure);
        };
        if token.is_ended() {
            return Err(failure);
        }
        context
            .log
            .event("identity-retry")
            .field("serial", serial)
            .field("error", &failure.to_string())
            .field("delayMs", &delay.as_millis())
            .write();
        if token.guard(serial, time::sleep(delay)).await.is_err() {
            return Err(failure);
        }
    }
}

/// Reads the phone's health, and its identity too when a periodic round finds it due: none
/// yet, SIM states that differ from it, or 30 minutes old.
async fn read_health(
    context: &ManagerContext,
    serial: &str,
    token: &SessionToken,
    reason: ProbeReason,
) -> Result<(), ProbeFailure> {
    if token.is_ended() {
        return Ok(());
    }
    let health = run_script(context, serial, token, &spareline_core::health_command()).await?;
    let health: PhoneHealth = spareline_core::parse_health(&health);
    if token.is_ended() {
        return Ok(());
    }
    let now = context.devices.clock().now();
    let due = {
        let mut table = context.devices.table();
        let phone = table.get(serial);
        let due = reason == ProbeReason::Health
            && phone.as_ref().is_some_and(|phone| {
                spareline_core::identity_due(
                    phone.identity.as_ref(),
                    phone.identity_at,
                    &health,
                    now,
                )
            });
        table.set_health(serial, token.id(), health, now);
        due
    };
    if due {
        read_identity(context, serial, token).await?;
    }
    Ok(())
}

/// Reads the phone's identity, keeps it, and records it in the inventory, announcing a change
/// of SIMs or numbers.
async fn read_identity(
    context: &ManagerContext,
    serial: &str,
    token: &SessionToken,
) -> Result<(), ProbeFailure> {
    let first = run_script(context, serial, token, &spareline_core::identity_command()).await?;
    let probe: IdentityProbe = spareline_core::parse_identity(&first);
    let identity = match probe.subinfo_command() {
        Some(command) => {
            let second = run_script(context, serial, token, &command).await?;
            probe.apply_subinfo(&second)
        }
        None => probe.into_identity(),
    };
    if token.is_ended() {
        return Ok(());
    }
    let now = context.devices.clock().now();
    context
        .devices
        .table()
        .set_identity(serial, token.id(), identity.clone(), now);
    let reading = identity_reading(&identity);
    let owned_serial = serial.to_owned();
    let buses = context.buses.clone();
    let recorded = context
        .db
        .call(
            move |db| -> Result<Option<Vec<DeviceEvent>>, DatabaseError> {
                let Some(phone_id) = db.inventory().phone_id(&owned_serial)? else {
                    return Ok(None);
                };
                let events = db.inventory().apply_identity(phone_id, &reading, now)?;
                let recorded = db.device_events().insert(&events, now)?;
                buses.publish_device_events(&recorded);
                Ok(Some(recorded))
            },
        )
        .await?;
    let Some(recorded) = recorded else {
        return Ok(());
    };
    if !identity.warnings.is_empty() {
        context
            .log
            .event("identity-warnings")
            .field("serial", serial)
            .field("warnings", &identity.warnings)
            .write();
    }
    let subs_changed = recorded
        .iter()
        .any(|event| event.kind.starts_with("sim.") || event.kind.starts_with("number."));
    if subs_changed {
        context.devices.emit(PhoneEvent::SubsChanged {
            serial: serial.to_owned(),
        });
    }
    Ok(())
}

/// Runs a probe script within the session and returns its stdout as text.
async fn run_script(
    context: &ManagerContext,
    serial: &str,
    token: &SessionToken,
    script: &str,
) -> Result<String, ProbeFailure> {
    let output = context
        .devices
        .shell_in(token, serial, script, PROBE_TIMEOUT)
        .await?;
    spareline_core::check_probe_exit(
        i32::from(output.exit_code),
        &shell_text::shell_text(&output.stderr),
    )?;
    Ok(shell_text::shell_text(&output.stdout))
}

/// What inventory records of an identity: the device facts, the first two IMEIs, and each
/// slot's SIM.
fn identity_reading(identity: &PhoneIdentity) -> IdentityReading {
    IdentityReading {
        manufacturer: identity.manufacturer.clone(),
        model: identity.model.clone(),
        android_version: identity.android_version.clone(),
        sdk: identity.sdk.and_then(|sdk| i64::try_from(sdk).ok()),
        fingerprint: identity.fingerprint.clone(),
        imei1: identity.imeis.first().cloned().flatten(),
        imei2: identity.imeis.get(1).cloned().flatten(),
        slots: identity
            .slots
            .iter()
            .map(|slot| SlotReading {
                slot: i64::from(slot.slot),
                contents: match slot.state {
                    SlotPresence::Empty => SlotContents::Empty,
                    SlotPresence::Unknown => SlotContents::Unknown,
                    SlotPresence::Present => SlotContents::Present(
                        identity
                            .sims
                            .iter()
                            .find(|sim| sim.slot == slot.slot)
                            .map(|sim| SimReading {
                                sub_id: sim.sub_id,
                                iccid: sim.iccid.clone(),
                                carrier: sim.carrier.clone(),
                                number: sim.number.clone(),
                            }),
                    ),
                },
            })
            .collect(),
    }
}
