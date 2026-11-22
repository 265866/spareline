//! The check every `/api/` request passes before its route runs.

use axum::extract::{Request, State};
use axum::http::{HeaderValue, header};
use axum::middleware::Next;
use axum::response::Response;
use spareline_db::{KeyScope, Principal};

use crate::api_error::ApiError;
use crate::app_state::AppState;
use crate::credentials::{self, Credentials, Transport};

/// Resolves who an `/api/` request acts for, from an `Authorization: Bearer` API key or else
/// the session cookie, and turns away what that principal may not do, as spareline-old's guard
/// did before routing:
///
/// - no valid credentials: 401, except on the public paths, which go on anonymously;
/// - an API key on the key and password routes: 403;
/// - a read key on anything but a `GET` or `HEAD` without a WebSocket upgrade: 403.
///
/// The principal goes into the request's extensions. A session used an hour or more after it
/// was last seen is extended, and the response carries a fresh cookie unless the route set one.
/// Requests outside `/api/` pass untouched.
pub(crate) async fn guard(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    use axum::response::IntoResponse as _;

    let path = request.uri().path().to_owned();
    if !path.starts_with("/api/") {
        return next.run(request).await;
    }
    let credentials = Credentials::from_headers(request.headers());
    let now = state.clock().now();
    let resolved = match credentials.clone() {
        Credentials::Key(key) => state
            .db()
            .call(move |db| db.accounts().key_principal(&key, now))
            .await
            .map(|found| {
                found.map(|key| Resolved {
                    principal: Principal::Key(key),
                    renewed_token: None,
                })
            }),
        Credentials::Session(token) => {
            state
                .db()
                .call(move |db| {
                    db.accounts().session(&token, now).map(|found| {
                        found.map(|lookup| Resolved {
                            principal: Principal::Session(lookup.principal),
                            renewed_token: lookup.renewed.then_some(token),
                        })
                    })
                })
                .await
        }
        Credentials::None => Ok(None),
    };
    let resolved = match resolved {
        Ok(resolved) => resolved,
        Err(error) => return ApiError::internal(state.log(), &error).into_response(),
    };
    let Some(Resolved {
        principal,
        renewed_token,
    }) = resolved
    else {
        if credentials::is_public(&path) {
            return next.run(request).await;
        }
        let message = match credentials {
            Credentials::Key(_) => "unknown or revoked API key",
            Credentials::Session(_) | Credentials::None => "sign in first",
        };
        return ApiError::unauthorized(message.to_owned()).into_response();
    };
    if let Principal::Key(key) = &principal {
        if credentials::is_session_only(&path) {
            return ApiError::forbidden(
                "API keys cannot manage keys or passwords; sign in with a browser".to_owned(),
            )
            .into_response();
        }
        if key.scope == KeyScope::Read
            && !credentials::is_read_only(request.method(), request.headers())
        {
            return ApiError::forbidden("this API key can only read".to_owned()).into_response();
        }
    }
    let transport = Transport::of(request.headers());
    request.extensions_mut().insert(principal);
    let mut response = next.run(request).await;
    if let Some(token) = renewed_token
        && !response.headers().contains_key(header::SET_COOKIE)
    {
        let cookie = credentials::session_cookie(&token, credentials::SESSION_MAX_AGE, transport);
        if let Ok(value) = HeaderValue::from_str(&cookie) {
            response.headers_mut().insert(header::SET_COOKIE, value);
        }
    }
    response
}

/// A request's principal, and its session token when this use renewed the session.
struct Resolved {
    principal: Principal,
    renewed_token: Option<String>,
}

/// The name an edit or a screen session is recorded under: the user's name, or
/// `<user> (key <key name>)` for an API key.
pub(crate) fn login_of(principal: &Principal) -> String {
    match principal {
        Principal::Session(session) => session.user.name.clone(),
        Principal::Key(key) => format!("{} (key {})", key.user.name, key.key_name),
    }
}
