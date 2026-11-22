//! Sign-in, who-am-I, sign-out and the password change: `/api/v1/auth/*`.

use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json, Router, routing};
use serde::{Deserialize, Serialize};
use spareline_db::{AccountError, Principal, SessionPrincipal, User};
use tokio::sync::OwnedSemaphorePermit;
use tokio::task;

use crate::api_error::ApiError;
use crate::app_state::AppState;
use crate::credentials::{self, Transport};
use crate::http::json_body::{self, JsonBody, Text};
use crate::login_limiter::LoginLimiter;

/// The hint `/api/v1/auth/me` gives when nobody can sign in yet, naming the command the web
/// UI's sign-in page shows.
const NO_USERS_HINT: &str =
    "no users yet: create one on the server with `spareline users add NAME`";

/// The routes under `/api/v1/auth`.
pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/auth/login", routing::post(login))
        .route("/api/v1/auth/me", routing::get(me))
        .route("/api/v1/auth/logout", routing::post(logout))
        .route("/api/v1/auth/password", routing::post(password))
}

/// `{"user": {"name": ...}}`.
#[derive(Serialize)]
struct Me<'user> {
    user: UserName<'user>,
}

#[derive(Serialize)]
struct UserName<'user> {
    name: &'user str,
}

impl<'user> Me<'user> {
    fn of(user: &'user User) -> Self {
        Self {
            user: UserName { name: &user.name },
        }
    }
}

/// `{"ok": true}`.
#[derive(Serialize)]
struct Done {
    ok: bool,
}

const DONE: Done = Done { ok: true };

/// The body of `POST /api/v1/auth/login`. Fields it doesn't name are ignored. Its name shows in
/// the messages that refuse it, as in the live build.
#[derive(Debug, Deserialize)]
struct Credentials {
    /// The user name.
    #[serde(deserialize_with = "json_body::non_null")]
    name: Text<1, 64>,
    /// The password.
    #[serde(deserialize_with = "json_body::non_null")]
    password: Text<1, 1024>,
}

/// The body of `POST /api/v1/auth/password`. Fields it doesn't name are ignored.
#[derive(Debug, Deserialize)]
struct PasswordChange {
    /// The password now.
    #[serde(deserialize_with = "json_body::non_null")]
    current: Text<1, 1024>,
    /// The new password, whose minimum length is checked after the current one.
    #[serde(deserialize_with = "json_body::non_null")]
    next: Text<0, 1024>,
}

/// `POST /api/v1/auth/login`: checks the name and password, rate limited per name, and sets
/// the session cookie.
async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    JsonBody(body): JsonBody<Credentials>,
) -> Response {
    login_response(&state, &headers, body)
        .await
        .unwrap_or_else(IntoResponse::into_response)
}

async fn login_response(
    state: &AppState,
    headers: &HeaderMap,
    body: Credentials,
) -> Result<Response, ApiError> {
    let name = body.name.0;
    let password = body.password.0;
    let keys = vec![LoginLimiter::key_for(&name)];
    if let Some(wait) = limit(state, &keys) {
        return Ok(too_many(
            wait,
            &format!("too many failed sign-ins; try again in {wait} s"),
        ));
    }
    let lookup = name.clone();
    let candidate = state
        .db()
        .call(move |db| db.accounts().login_candidate(&lookup))
        .await
        .map_err(|error| ApiError::internal(state.log(), &error))?;
    let verifier = state.clone();
    let permit = hash_permit(state).await?;
    let user = task::spawn_blocking(move || {
        let _permit = permit;
        verifier.passwords().verify_login(candidate, &password)
    })
    .await
    .map_err(|error| ApiError::internal(state.log(), &error))?
    .map_err(|error| ApiError::internal(state.log(), &error))?;
    let Some(user) = user else {
        return Err(ApiError::unauthorized("wrong name or password".to_owned()));
    };
    succeed(state, &keys);
    let now = state.clock().now();
    let user_id = user.id;
    let token = state
        .db()
        .call(move |db| db.accounts().create_session(user_id, now))
        .await
        .map_err(|error| ApiError::internal(state.log(), &error))?;
    let cookie =
        credentials::session_cookie(&token, credentials::SESSION_MAX_AGE, Transport::of(headers));
    Ok(with_cookie(&cookie, Json(Me::of(&user)).into_response()))
}

/// `GET /api/v1/auth/me`: the signed-in user, or a 401 that says whether any user exists.
async fn me(State(state): State<AppState>, principal: Option<Extension<Principal>>) -> Response {
    if let Some(principal) = principal.map(|found| found.0) {
        return Json(Me::of(principal.user())).into_response();
    }
    match state.db().call(|db| db.accounts().has_users()).await {
        Ok(has_users) => {
            let error = if has_users {
                "sign in first"
            } else {
                NO_USERS_HINT
            };
            (
                StatusCode::UNAUTHORIZED,
                Json(NotSignedIn {
                    error,
                    no_users: !has_users,
                }),
            )
                .into_response()
        }
        Err(error) => ApiError::internal(state.log(), &error).into_response(),
    }
}

/// The 401 body of `/api/v1/auth/me`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NotSignedIn<'text> {
    error: &'text str,
    no_users: bool,
}

/// `POST /api/v1/auth/logout`: ends this session and clears the cookie.
async fn logout(
    State(state): State<AppState>,
    principal: Option<Extension<Principal>>,
    headers: HeaderMap,
) -> Response {
    if let Err(error) = json_body::require_json(&headers) {
        return error.into_response();
    }
    if let Some(Extension(Principal::Session(session))) = principal {
        let session_id = session.session_id;
        if let Err(error) = state
            .db()
            .call(move |db| db.accounts().end_session(session_id))
            .await
        {
            return ApiError::internal(state.log(), &error).into_response();
        }
        state.buses().publish_auth_change();
    }
    let cookie = credentials::session_cookie("", 0, Transport::of(&headers));
    with_cookie(&cookie, Json(DONE).into_response())
}

/// `POST /api/v1/auth/password`: checks the current password, rate limited with sign-ins,
/// sets the new one and ends every other session of the user.
async fn password(
    State(state): State<AppState>,
    principal: Option<Extension<Principal>>,
    JsonBody(body): JsonBody<PasswordChange>,
) -> Response {
    let Some(Extension(Principal::Session(session))) = principal else {
        return ApiError::unauthorized("sign in first".to_owned()).into_response();
    };
    change_password(&state, session, body)
        .await
        .unwrap_or_else(IntoResponse::into_response)
}

async fn change_password(
    state: &AppState,
    session: SessionPrincipal,
    body: PasswordChange,
) -> Result<Response, ApiError> {
    let current = body.current.0;
    let next = body.next.0;
    let keys = vec![LoginLimiter::key_for(&session.user.name)];
    if let Some(wait) = limit(state, &keys) {
        return Ok(too_many(
            wait,
            &format!("too many failed attempts; try again in {wait} s"),
        ));
    }
    let name = session.user.name.clone();
    let candidate = state
        .db()
        .call(move |db| db.accounts().login_candidate(&name))
        .await
        .map_err(|error| ApiError::internal(state.log(), &error))?;
    let verifier = state.clone();
    let permit = hash_permit(state).await?;
    let hashed = task::spawn_blocking(move || {
        let _permit = permit;
        let user = verifier.passwords().verify_login(candidate, &current)?;
        if user.is_none() {
            return Ok(None);
        }
        verifier.passwords().hash_new(&next).map(Some)
    })
    .await
    .map_err(|error| ApiError::internal(state.log(), &error))?;
    let hash = match hashed {
        Ok(Some(hash)) => hash,
        Ok(None) => {
            return Err(ApiError::forbidden(
                "the current password is wrong".to_owned(),
            ));
        }
        Err(AccountError::PasswordTooShort) => {
            succeed(state, &keys);
            return Err(ApiError::bad_request(
                AccountError::PasswordTooShort.to_string(),
            ));
        }
        Err(error) => return Err(ApiError::internal(state.log(), &error)),
    };
    succeed(state, &keys);
    let now = state.clock().now();
    let user_id = session.user.id;
    let keep = session.session_id;
    state
        .db()
        .call(move |db| db.accounts().set_password(user_id, &hash, Some(keep), now))
        .await
        .map_err(|error| ApiError::internal(state.log(), &error))?;
    state.buses().publish_auth_change();
    Ok(Json(DONE).into_response())
}

/// Waits for a place to hash a password, which the hashing task holds until it ends. Waiting
/// here rather than on a blocking thread keeps a flood of sign-ins from taking every blocking
/// thread, which the database calls need too.
async fn hash_permit(state: &AppState) -> Result<OwnedSemaphorePermit, ApiError> {
    state
        .hash_permit()
        .await
        .map_err(|error| ApiError::internal(state.log(), &error))
}

/// Checks the limiter for `keys` and, when they aren't locked out, records this attempt as a
/// failure until it succeeds, in one step, so concurrent attempts can't all get past the check.
/// Returns the seconds to wait when locked out.
fn limit(state: &AppState, keys: &[String]) -> Option<u64> {
    let now = state.clock().now();
    state
        .limiter()
        .lock()
        .expect("no request panicked while holding the sign-in limiter")
        .begin(keys, now)
        .err()
}

/// Clears the failures of `keys` after a correct password.
fn succeed(state: &AppState, keys: &[String]) {
    state
        .limiter()
        .lock()
        .expect("no request panicked while holding the sign-in limiter")
        .succeed(keys);
}

/// A 429 with `Retry-After`.
fn too_many(wait: u64, message: &str) -> Response {
    let mut response =
        ApiError::new(StatusCode::TOO_MANY_REQUESTS, message.to_owned()).into_response();
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from(wait));
    response
}

/// `response` with `Set-Cookie: cookie`.
fn with_cookie(cookie: &str, mut response: Response) -> Response {
    if let Ok(value) = HeaderValue::from_str(cookie) {
        response.headers_mut().insert(header::SET_COOKIE, value);
    }
    response
}
