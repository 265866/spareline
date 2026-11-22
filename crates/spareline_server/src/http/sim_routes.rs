//! SIMs: `/api/v1/sims`.

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json, Router, routing};
use serde::{Deserialize, Serialize};
use spareline_db::{InventoryError, Principal, SimPatch, SimRecord, SimStatus};

use crate::api_error::ApiError;
use crate::app_state::AppState;
use crate::http::api_path::ApiPath;
use crate::http::json_body::JsonBody;
use crate::http::{guard, json_body};

/// The routes under `/api/v1/sims`.
pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/sims", routing::get(list))
        .route("/api/v1/sims/{key}", routing::get(one).patch(patch))
}

/// `{"sims": [...]}`.
#[derive(Serialize)]
struct SimList {
    sims: Vec<SimRecord>,
}

/// `GET /api/v1/sims`: every SIM, by id.
async fn list(State(state): State<AppState>) -> Response {
    match state.db().call(|db| db.inventory().list_sims()).await {
        Ok(sims) => Json(SimList { sims }).into_response(),
        Err(error) => ApiError::internal(state.log(), &error).into_response(),
    }
}

/// `GET /api/v1/sims/{key}`: one SIM, by id (up to 9 digits) or ICCID.
async fn one(State(state): State<AppState>, ApiPath(key): ApiPath<String>) -> Response {
    let lookup = key.clone();
    match state
        .db()
        .call(move |db| db.inventory().find_sim(&lookup))
        .await
    {
        Ok(Some(sim)) => Json(sim).into_response(),
        Ok(None) => ApiError::not_found(format!("no SIM {key}")).into_response(),
        Err(error) => ApiError::internal(state.log(), &error).into_response(),
    }
}

/// The body of `PATCH /api/v1/sims/{key}`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SimPatchBody {
    /// The new status; it may be left out but not `null`.
    #[serde(default, deserialize_with = "json_body::optional_non_null")]
    status: Option<StatusName>,
}

/// A SIM status as a `PATCH` body names it.
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum StatusName {
    /// In use.
    Active,
    /// Kept in reserve.
    Spare,
    /// Out of use for good.
    Retired,
}

impl StatusName {
    /// The status it names.
    fn status(self) -> SimStatus {
        match self {
            Self::Active => SimStatus::Active,
            Self::Spare => SimStatus::Spare,
            Self::Retired => SimStatus::Retired,
        }
    }
}

/// `PATCH /api/v1/sims/{key}`: sets a SIM's status, recording who did. The body is checked
/// before the SIM is looked up.
async fn patch(
    State(state): State<AppState>,
    principal: Option<Extension<Principal>>,
    ApiPath(key): ApiPath<String>,
    JsonBody(body): JsonBody<SimPatchBody>,
) -> Response {
    let sim_patch = SimPatch {
        status: body.status.map(StatusName::status),
    };
    let login = principal
        .map(|found| found.0)
        .map(|principal| guard::login_of(&principal));
    let now = state.clock().now();
    let lookup = key.clone();
    let result = state
        .db()
        .call(move |db| {
            let Some(sim) = db.inventory().find_sim(&lookup)? else {
                return Ok(None);
            };
            db.inventory()
                .patch_sim(sim.id, &sim_patch, login.as_deref(), now)
                .map(Some)
        })
        .await;
    match result {
        Ok(Some(sim)) => Json(sim).into_response(),
        Ok(None) | Err(InventoryError::NoSim { .. }) => {
            ApiError::not_found(format!("no SIM {key}")).into_response()
        }
        Err(error) => ApiError::internal(state.log(), &error).into_response(),
    }
}
