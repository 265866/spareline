//! Phone numbers: `/api/v1/numbers`.

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json, Router, routing};
use serde::{Deserialize, Serialize};
use spareline_core::RadioState;
use spareline_db::{
    FieldPatch, InventoryError, LastMessage, NumberPatch, NumberPhone, NumberRecord, NumberSource,
    Principal, SimSummary,
};

use crate::api_error::ApiError;
use crate::app_state::AppState;
use crate::http::api_path::ApiPath;
use crate::http::json_body::{JsonBody, Text};
use crate::http::{guard, json_body, phone_view};

/// The routes under `/api/v1/numbers`, but the texts.
pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/numbers", routing::get(list))
        .route("/api/v1/numbers/{key}", routing::get(one).patch(patch))
}

/// The API's `PhoneNumber`, with its fields in the live build's order: the live `phoneStatus`
/// and `radio` come last, after `lastMessage`, though the `OpenAPI` document lists them between
/// `phone` and `messageCount`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct NumberView {
    id: i64,
    e164: Option<String>,
    source: NumberSource,
    note: String,
    #[serde(rename = "detectedE164")]
    detected_e164: Option<String>,
    confirmed_by: Option<String>,
    created_at: String,
    sim: Option<SimSummary>,
    phone: Option<NumberPhone>,
    message_count: i64,
    last_message: Option<LastMessage>,
    phone_status: Option<&'static str>,
    radio: Option<RadioState>,
}

impl NumberView {
    /// The number with its phone's live status and its slot's radio. A number whose SIM left its
    /// last phone has neither; one whose phone this process hasn't seen is `offline`.
    fn new(record: NumberRecord, state: &AppState) -> Self {
        let present = record
            .phone
            .as_ref()
            .filter(|phone| phone.removed_at.is_none());
        let live = present.and_then(|phone| state.devices().get(&phone.serial));
        let phone_status =
            present.map(|_| live.as_ref().map_or("offline", |live| live.status.as_str()));
        let radio = match (&live, &record.phone) {
            (Some(live), Some(phone)) => live
                .health
                .as_ref()
                .and_then(|health| phone_view::radio_of(&health.radios, phone.slot)),
            (Some(_) | None, None) | (None, Some(_)) => None,
        };
        Self {
            id: record.id,
            e164: record.e164,
            source: record.source,
            note: record.note,
            detected_e164: record.detected_e164,
            confirmed_by: record.confirmed_by,
            created_at: record.created_at,
            sim: record.sim,
            phone: record.phone,
            message_count: record.message_count,
            last_message: record.last_message,
            phone_status,
            radio,
        }
    }
}

/// `{"numbers": [...]}`.
#[derive(Serialize)]
struct NumberList {
    numbers: Vec<NumberView>,
}

/// `GET /api/v1/numbers`: every number, those with an E.164 value first.
async fn list(State(state): State<AppState>) -> Response {
    match state.db().call(|db| db.inventory().list_numbers()).await {
        Ok(records) => {
            let numbers: Vec<NumberView> = records
                .into_iter()
                .map(|record| NumberView::new(record, &state))
                .collect();
            Json(NumberList { numbers }).into_response()
        }
        Err(error) => ApiError::internal(state.log(), &error).into_response(),
    }
}

/// `GET /api/v1/numbers/{key}`: one number, by id (up to 9 digits) or E.164.
async fn one(State(state): State<AppState>, ApiPath(key): ApiPath<String>) -> Response {
    let lookup = key.clone();
    match state
        .db()
        .call(move |db| db.inventory().find_number(&lookup))
        .await
    {
        Ok(Some(record)) => Json(NumberView::new(record, &state)).into_response(),
        Ok(None) => ApiError::not_found(format!("no number {key}")).into_response(),
        Err(error) => ApiError::internal(state.log(), &error).into_response(),
    }
}

/// The body of `PATCH /api/v1/numbers/{key}`, checked but not yet normalized.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NumberPatchBody {
    /// The new note; it may be left out but not `null`.
    #[serde(default, deserialize_with = "json_body::optional_non_null")]
    note: Option<Text<0, 2000>>,
    /// The number set by hand, or `null` to return to the one its SIM reports.
    #[serde(default, deserialize_with = "json_body::nullable")]
    e164: FieldPatch<Text<0, 32>>,
}

/// `PATCH /api/v1/numbers/{key}`: edits a number's note, or sets the number by hand; `null`
/// returns it to the one its SIM reports. A number typed without a country code is read as a US
/// number. The body is checked before the number is looked up.
async fn patch(
    State(state): State<AppState>,
    principal: Option<Extension<Principal>>,
    ApiPath(key): ApiPath<String>,
    JsonBody(edit): JsonBody<NumberPatchBody>,
) -> Response {
    let lookup = key.clone();
    let id = match state
        .db()
        .call(move |db| db.inventory().number_id_of(&lookup))
        .await
    {
        Ok(Some(id)) => id,
        Ok(None) => return ApiError::not_found(format!("no number {key}")).into_response(),
        Err(error) => return ApiError::internal(state.log(), &error).into_response(),
    };
    let e164 = match json_body::map_patch(edit.e164, |text| text.0) {
        FieldPatch::Value(raw) => match spareline_core::to_e164(&raw, Some("us")) {
            Some(e164) => FieldPatch::Value(e164),
            None => {
                return ApiError::bad_request(format!(
                    "\"{raw}\" is not a phone number in E.164 or 10-digit US form"
                ))
                .into_response();
            }
        },
        FieldPatch::Null => FieldPatch::Null,
        FieldPatch::Absent => FieldPatch::Absent,
    };
    let number_patch = NumberPatch {
        note: edit.note.map(|text| text.0),
        e164,
    };
    let login = principal
        .map(|found| found.0)
        .map(|principal| guard::login_of(&principal));
    let now = state.clock().now();
    match state
        .db()
        .call(move |db| {
            db.inventory()
                .patch_number(id, &number_patch, login.as_deref(), now)
        })
        .await
    {
        Ok(record) => Json(NumberView::new(record, &state)).into_response(),
        Err(error @ InventoryError::Conflict { .. }) => {
            ApiError::conflict(error.to_string()).into_response()
        }
        Err(InventoryError::NoNumber { .. }) => {
            ApiError::not_found(format!("no number {key}")).into_response()
        }
        Err(error) => ApiError::internal(state.log(), &error).into_response(),
    }
}
