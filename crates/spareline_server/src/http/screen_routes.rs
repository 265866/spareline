//! The phones' screens: `GET /api/v1/screens` and the screen WebSocket.

use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::{State, WebSocketUpgrade};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json, Router, routing};
use serde::Serialize;
use spareline_db::Principal;

use crate::app_state::AppState;
use crate::http::api_path::ApiPath;
use crate::http::guard;
use crate::http::revocation::Revocation;
use crate::screen::{SCREEN_SOCKET_LIMIT, ScreenSession};

/// The screen routes.
pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/screens", routing::get(list))
        .route("/api/v1/phones/{serial}/screen", routing::get(screen))
}

/// `{"sessions": [...]}`.
#[derive(Serialize)]
struct SessionList {
    sessions: Vec<ScreenSession>,
}

/// `GET /api/v1/screens`: every phone with a screen session or viewers.
async fn list(State(state): State<AppState>) -> Response {
    Json(SessionList {
        sessions: state.screens().list(),
    })
    .into_response()
}

/// `GET /api/v1/phones/{serial}/screen`, a WebSocket: the phone's live screen, and control of
/// it. A browser's `Origin` must name this host, so a page from another site, open in a
/// signed-in browser, can't watch or control a phone; a client without `Origin` is allowed.
/// The socket runs as one of [`AppState::spawn_socket`]'s tasks, which the server waits for and
/// then drops at shutdown.
async fn screen(
    State(state): State<AppState>,
    principal: Option<Extension<Principal>>,
    ApiPath(serial): ApiPath<String>,
    headers: HeaderMap,
    upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    let origin = headers
        .get(header::ORIGIN)
        .map(|value| String::from_utf8_lossy(value.as_bytes()).into_owned());
    let host = headers
        .get(header::HOST)
        .map(|value| String::from_utf8_lossy(value.as_bytes()).into_owned());
    if !same_origin(origin.as_deref(), host.as_deref()) {
        return (
            StatusCode::FORBIDDEN,
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/plain; charset=utf-8"),
            )],
            "cross-origin WebSocket refused",
        )
            .into_response();
    }
    let upgrade = match upgrade {
        Ok(upgrade) => upgrade,
        Err(rejection) => return rejection.into_response(),
    };
    let Some(Extension(principal)) = principal else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let login = guard::login_of(&principal);
    let revocation = Revocation::watch(&state, principal);
    let screens = state.screens().clone();
    upgrade
        .max_message_size(SCREEN_SOCKET_LIMIT)
        .max_frame_size(SCREEN_SOCKET_LIMIT)
        .on_upgrade(move |socket| async move {
            state.spawn_socket(async move {
                screens
                    .attach(&serial, socket, login, revocation.revoked())
                    .await;
            });
        })
}

/// Whether a WebSocket's `Origin` names the host the request was sent to: allowed without an
/// `Origin`; otherwise the origin's host and port, as JavaScript's `new URL(origin).host` reads
/// them, must equal the `Host` header in lowercase. Reverse proxies such as Tailscale Serve pass
/// both through unchanged.
fn same_origin(origin: Option<&str>, host: Option<&str>) -> bool {
    let Some(origin) = origin else {
        return true;
    };
    match (url_host(origin), host) {
        (Some(origin_host), Some(host)) => origin_host == host.to_lowercase(),
        (Some(_) | None, None) | (None, Some(_)) => false,
    }
}

/// The `host` of the URL `text`: its host in lowercase and its port, unless the port is the
/// scheme's default. `None` for text that isn't an absolute URL with a host, such as `null`.
fn url_host(text: &str) -> Option<String> {
    let (scheme, rest) = text.split_once("://")?;
    let valid_scheme = scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    if !valid_scheme {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let split_at = if authority.starts_with('[') {
        authority.find(']').map(|end| end + 1)
    } else {
        authority.find(':')
    };
    let (name, port) = match split_at {
        Some(index) => {
            let (name, port) = authority.split_at(index);
            (name, port.strip_prefix(':').unwrap_or(port))
        }
        None => (authority, ""),
    };
    if name.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let default_port = match scheme.to_ascii_lowercase().as_str() {
        "http" | "ws" => Some("80"),
        "https" | "wss" => Some("443"),
        _ => None,
    };
    let name = name.to_lowercase();
    if port.is_empty() || Some(port) == default_port {
        Some(name)
    } else {
        Some(format!("{name}:{port}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_origin_compares_the_origins_host_and_port_with_host() {
        assert!(same_origin(None, Some("a.example")));
        assert!(same_origin(Some("https://a.example"), Some("a.example")));
        assert!(same_origin(
            Some("https://a.example:10000"),
            Some("a.example:10000")
        ));
        assert!(same_origin(Some("https://A.example"), Some("a.EXAMPLE")));
        assert!(same_origin(
            Some("https://a.example:443"),
            Some("a.example")
        ));
        assert!(same_origin(Some("http://[::1]:8740"), Some("[::1]:8740")));
        assert!(!same_origin(
            Some("https://a.example"),
            Some("a.example:10000")
        ));
        assert!(!same_origin(Some("https://b.example"), Some("a.example")));
        assert!(!same_origin(Some("null"), Some("a.example")));
        assert!(!same_origin(Some("https://a.example"), None));
        assert!(!same_origin(
            Some("http://127.0.0.1:1"),
            Some("127.0.0.1:8740")
        ));
    }
}
