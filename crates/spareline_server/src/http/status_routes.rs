//! Liveness and live state: `GET /healthz`, `GET /api/v1/health` and `GET /api/v1/status`.

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::{Json, Router, routing};
use serde::{Serialize, Serializer};
use spareline_core::monitor::{
    Alert, Busy, CheckReport, PhonePolicy, ProfileSummary, ProvisionReport,
};
use spareline_core::{Battery, RadioState, Timestamp};
use spareline_db::PhoneStatus;

use crate::api_error::ApiError;
use crate::app_state::AppState;
use crate::devices::{AdbStatus, LivePhone};
use crate::http::phone_view;

/// The liveness and status routes.
pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/healthz", routing::get(healthz))
        .route("/api/v1/health", routing::get(health))
        .route("/api/v1/status", routing::get(status))
}

/// `{"ok": ...}`.
#[derive(Serialize)]
struct Liveness {
    ok: bool,
}

/// `GET /healthz`, public: whether the adb server is reachable. Always 200 while the process
/// serves, so a stopped adb server doesn't fail the container's health check.
async fn healthz(State(state): State<AppState>) -> Response {
    Json(Liveness {
        ok: state.devices().adb().reachable,
    })
    .into_response()
}

/// The API's `Health`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Health<'version> {
    ok: bool,
    adb_reachable: bool,
    phones_online: usize,
    phones_offline: usize,
    alerts: usize,
    version: &'version str,
}

/// `GET /api/v1/health`: whether adb answers, how many stored phones are online and offline,
/// and how many alerts are open.
async fn health(State(state): State<AppState>) -> Response {
    let known = match state.db().call(|db| db.inventory().list_phones()).await {
        Ok(known) => known,
        Err(error) => return ApiError::internal(state.log(), &error).into_response(),
    };
    let reachable = state.devices().adb().reachable;
    let online = known
        .iter()
        .filter(|phone| {
            state
                .devices()
                .get(&phone.serial)
                .is_some_and(|live| live.status.as_str() == PhoneStatus::Online.as_str())
        })
        .count();
    Json(Health {
        ok: reachable,
        adb_reachable: reachable,
        phones_online: online,
        phones_offline: known.len() - online,
        alerts: state.monitor().open_alerts(),
        version: state.version(),
    })
    .into_response()
}

/// The API's `Status`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StatusView {
    adb: AdbView,
    phones: Vec<StatusPhone>,
    alerts: Vec<Alert>,
    data_threshold_bytes: Option<u64>,
    profile: Option<ProfileSummary>,
}

/// The adb server's reachability: `{reachable, version, since, error}`.
#[derive(Serialize)]
struct AdbView {
    reachable: bool,
    version: Option<u32>,
    #[serde(serialize_with = "time")]
    since: Timestamp,
    error: Option<String>,
}

impl AdbView {
    fn new(adb: AdbStatus) -> Self {
        Self {
            reachable: adb.reachable,
            version: adb.version,
            since: adb.since,
            error: adb.error,
        }
    }
}

/// One phone the phone manager knows this run.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StatusPhone {
    serial: String,
    status: &'static str,
    adb_state: &'static str,
    #[serde(serialize_with = "time")]
    since: Timestamp,
    usb: Option<String>,
    battery: Option<Battery>,
    radios: Vec<RadioState>,
    sim_states: Vec<String>,
    #[serde(serialize_with = "phone_view::optional_time")]
    health_at: Option<Timestamp>,
    #[serde(serialize_with = "phone_view::optional_time")]
    identity_at: Option<Timestamp>,
    probe_error: Option<String>,
    policy: Option<PolicyView>,
}

impl StatusPhone {
    fn new(phone: LivePhone, policy: Option<&PhonePolicy>) -> Self {
        let health = phone.health;
        Self {
            serial: phone.serial,
            status: phone.status.as_str(),
            adb_state: phone.adb_state.as_str(),
            since: phone.since,
            usb: phone.usb,
            battery: health.as_ref().and_then(|health| health.battery.clone()),
            radios: health
                .as_ref()
                .map_or_default(|health| health.radios.clone()),
            sim_states: health.map_or_default(|health| health.sim_states),
            health_at: phone.health_at,
            identity_at: phone.identity_at,
            probe_error: phone.probe_error,
            policy: policy.map(PolicyView::new),
        }
    }
}

/// The API's `PhonePolicy`: `{busy, check, provision, dataAlarm}`, without the serial the
/// monitor keeps it under, which the old schema left out.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PolicyView {
    busy: Option<Busy>,
    check: Option<CheckReport>,
    provision: Option<ProvisionReport>,
    data_alarm: bool,
}

impl PolicyView {
    fn new(policy: &PhonePolicy) -> Self {
        Self {
            busy: policy.busy,
            check: policy.check.clone(),
            provision: policy.provision.clone(),
            data_alarm: policy.data_alarm,
        }
    }
}

/// `GET /api/v1/status`: the adb server, every phone the phone manager knows this run with its
/// policy state, the open alerts, the data threshold and the profile.
async fn status(State(state): State<AppState>) -> Response {
    let monitor = state.monitor().status().await;
    let phones: Vec<StatusPhone> = state
        .devices()
        .list()
        .into_iter()
        .map(|phone| {
            let policy = monitor.policy(&phone.serial);
            StatusPhone::new(phone, policy)
        })
        .collect();
    Json(StatusView {
        adb: AdbView::new(state.devices().adb()),
        phones,
        alerts: monitor.alerts,
        data_threshold_bytes: monitor.data_threshold_bytes,
        profile: monitor.profile,
    })
    .into_response()
}

/// Writes a time in the 24-character form.
#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's serialize_with hands the function a reference to the field itself"
)]
fn time<S: Serializer>(time: &Timestamp, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_str(time)
}
