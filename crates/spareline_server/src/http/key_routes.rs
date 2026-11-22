//! Listing, creating and revoking API keys: `/api/v1/api-keys`, for signed-in browsers only.

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json, Router, routing};
use serde::{Deserialize, Serialize};
use spareline_db::{AccountError, ApiKeyView, KeyScope, Principal};

use crate::api_error::ApiError;
use crate::app_state::AppState;
use crate::http::api_path::ApiPath;
use crate::http::json_body::{self, JsonBody, Text};
use crate::http::query_params::IntRange;

/// The routes under `/api/v1/api-keys`.
pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/api-keys", routing::get(list).post(create))
        .route("/api/v1/api-keys/{id}", routing::delete(revoke))
}

/// `{"keys": [...]}`.
#[derive(Serialize)]
struct KeyList {
    keys: Vec<ApiKeyView>,
}

/// `GET /api/v1/api-keys`: every user's keys, revoked ones included, newest first.
async fn list(State(state): State<AppState>) -> Response {
    match state.db().call(|db| db.accounts().list_keys()).await {
        Ok(keys) => Json(KeyList { keys }).into_response(),
        Err(error) => ApiError::internal(state.log(), &error).into_response(),
    }
}

/// `POST /api/v1/api-keys`: makes a key for the signed-in user and shows it once.
async fn create(
    State(state): State<AppState>,
    principal: Option<Extension<Principal>>,
    JsonBody(body): JsonBody<KeyRequest>,
) -> Response {
    let Some(Extension(Principal::Session(session))) = principal else {
        return ApiError::unauthorized("sign in first".to_owned()).into_response();
    };
    let name = body.name.0;
    let scope = body.scope.scope();
    let now = state.clock().now();
    let user_id = session.user.id;
    match state
        .db()
        .call(move |db| db.accounts().create_key(user_id, &name, scope, now))
        .await
    {
        Ok(new_key) => Json(new_key).into_response(),
        Err(error) if error.is_user_error() => {
            ApiError::bad_request(error.to_string()).into_response()
        }
        Err(error) => ApiError::internal(state.log(), &error).into_response(),
    }
}

/// The body of `POST /api/v1/api-keys`. Fields it doesn't name are ignored.
#[derive(Debug, Deserialize)]
struct KeyRequest {
    /// The key's name; the store trims it and refuses it blank.
    #[serde(deserialize_with = "json_body::non_null")]
    name: Text<0, 64>,
    /// What the key may do.
    #[serde(deserialize_with = "json_body::non_null")]
    scope: ScopeName,
}

/// A key scope as the body names it.
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum ScopeName {
    /// `GET` and `HEAD` only.
    Read,
    /// Everything but the key and password routes.
    Full,
}

impl ScopeName {
    /// The scope it names.
    fn scope(self) -> KeyScope {
        match self {
            Self::Read => KeyScope::Read,
            Self::Full => KeyScope::Full,
        }
    }
}

/// `DELETE /api/v1/api-keys/{id}`: revokes the key; idempotent. Its open streams and screens
/// close.
async fn revoke(State(state): State<AppState>, ApiPath(raw): ApiPath<String>) -> Response {
    let id = match IntRange::any().check("id", &raw) {
        Ok(id) => id,
        Err(error) => return error.into_response(),
    };
    let now = state.clock().now();
    match state
        .db()
        .call(move |db| db.accounts().revoke_key(id, now))
        .await
    {
        Ok(key) => {
            state.buses().publish_auth_change();
            Json(key).into_response()
        }
        Err(error @ AccountError::NoApiKey { .. }) => {
            ApiError::not_found(error.to_string()).into_response()
        }
        Err(error) => ApiError::internal(state.log(), &error).into_response(),
    }
}
