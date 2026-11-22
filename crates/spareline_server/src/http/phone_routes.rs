//! Phones: `/api/v1/phones`.

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json, Router, routing};
use serde::{Deserialize, Serialize};
use spareline_db::{FieldPatch, InventoryError, PhonePatch, Principal};

use crate::api_error::ApiError;
use crate::app_state::AppState;
use crate::http::api_path::ApiPath;
use crate::http::json_body::{Cents, Date, JsonBody, Text};
use crate::http::phone_view::PhoneView;
use crate::http::{guard, json_body};

/// The routes under `/api/v1/phones`, but Set up phone, the check and the screen.
pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/phones", routing::get(list))
        .route("/api/v1/phones/{serial}", routing::get(one).patch(patch))
}

/// `{"phones": [...]}`.
#[derive(Serialize)]
struct PhoneList {
    phones: Vec<PhoneView>,
}

/// `GET /api/v1/phones`: every phone, by serial, with its live state.
async fn list(State(state): State<AppState>) -> Response {
    match state.db().call(|db| db.inventory().list_phones()).await {
        Ok(records) => {
            let phones: Vec<PhoneView> = records
                .into_iter()
                .map(|record| {
                    let live = state.devices().get(&record.serial);
                    PhoneView::new(record, live.as_ref())
                })
                .collect();
            Json(PhoneList { phones }).into_response()
        }
        Err(error) => ApiError::internal(state.log(), &error).into_response(),
    }
}

/// `GET /api/v1/phones/{serial}`: one phone.
async fn one(State(state): State<AppState>, ApiPath(serial): ApiPath<String>) -> Response {
    let lookup = serial.clone();
    match state
        .db()
        .call(move |db| db.inventory().phone(&lookup))
        .await
    {
        Ok(Some(record)) => {
            let live = state.devices().get(&record.serial);
            Json(PhoneView::new(record, live.as_ref())).into_response()
        }
        Ok(None) => ApiError::not_found(format!("no phone {serial}")).into_response(),
        Err(error) => ApiError::internal(state.log(), &error).into_response(),
    }
}

/// The body of `PATCH /api/v1/phones/{serial}`: each field may be left out, set, or `null` to
/// clear it.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PhonePatchBody {
    /// Where the phone sits.
    #[serde(default, deserialize_with = "json_body::nullable")]
    location: FieldPatch<Text<0, 64>>,
    /// The USB hub port it is plugged into.
    #[serde(default, deserialize_with = "json_body::nullable")]
    hub_port: FieldPatch<Text<0, 64>>,
    /// What it cost.
    #[serde(default, deserialize_with = "json_body::nullable")]
    cost_cents: FieldPatch<Cents>,
    /// When it was bought.
    #[serde(default, deserialize_with = "json_body::nullable")]
    acquired_on: FieldPatch<Date>,
}

/// `PATCH /api/v1/phones/{serial}`: edits a phone's location, hub port, cost and purchase date,
/// recording who did. The body is checked before the phone is looked up.
async fn patch(
    State(state): State<AppState>,
    principal: Option<Extension<Principal>>,
    ApiPath(serial): ApiPath<String>,
    JsonBody(body): JsonBody<PhonePatchBody>,
) -> Response {
    let phone_patch = PhonePatch {
        location: json_body::map_patch(body.location, |text| text.0),
        hub_port: json_body::map_patch(body.hub_port, |text| text.0),
        cost_cents: json_body::map_patch(body.cost_cents, |cents| cents.0),
        acquired_on: json_body::map_patch(body.acquired_on, |date| date.0),
    };
    let login = principal
        .map(|found| found.0)
        .map(|principal| guard::login_of(&principal));
    let now = state.clock().now();
    let lookup = serial.clone();
    match state
        .db()
        .call(move |db| {
            db.inventory()
                .patch_phone(&lookup, &phone_patch, login.as_deref(), now)
        })
        .await
    {
        Ok(record) => {
            let live = state.devices().get(&record.serial);
            Json(PhoneView::new(record, live.as_ref())).into_response()
        }
        Err(InventoryError::NoPhone { .. }) => {
            ApiError::not_found(format!("no phone {serial}")).into_response()
        }
        Err(error) => ApiError::internal(state.log(), &error).into_response(),
    }
}
