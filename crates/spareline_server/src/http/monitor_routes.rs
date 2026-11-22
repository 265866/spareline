//! Set up phone and the on-demand drift check: `POST /api/v1/phones/{serial}/provision` and
//! `.../check`.

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::{Json, Router, routing};

use crate::api_error::ApiError;
use crate::app_state::AppState;
use crate::http::api_path::ApiPath;
use crate::http::json_body;

/// The header a script may send instead of a JSON body.
const ACTION_HEADER: &str = "x-spareline";

/// The routes that act on a phone through the monitor.
pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/phones/{serial}/provision",
            routing::post(provision),
        )
        .route("/api/v1/phones/{serial}/check", routing::post(check))
}

/// Refuses a request a page on another site could make. A browser sends a cross-site form only
/// as form or text content, and a JSON body or a custom header from another origin needs a CORS
/// preflight this server never grants; `Sec-Fetch-Site` turns away the rest.
///
/// # Errors
///
/// A 403 naming what to send.
fn same_site_only(headers: &HeaderMap) -> Result<(), ApiError> {
    let cross_site = headers
        .get("sec-fetch-site")
        .is_some_and(|value| value.as_bytes() == b"cross-site");
    let marked = headers
        .get(ACTION_HEADER)
        .is_some_and(|value| value.as_bytes() == b"1");
    if cross_site || !(json_body::is_json(headers) || marked) {
        return Err(ApiError::forbidden(format!(
            "send Content-Type: application/json or {ACTION_HEADER}: 1, from this site"
        )));
    }
    Ok(())
}

/// `POST /api/v1/phones/{serial}/provision`: sets the phone up to match the policy.
async fn provision(
    State(state): State<AppState>,
    ApiPath(serial): ApiPath<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(error) = same_site_only(&headers) {
        return error.into_response();
    }
    match state.monitor().provision(&serial).await {
        Ok(report) => Json(report).into_response(),
        Err(error) => ApiError::conflict(error.to_string()).into_response(),
    }
}

/// `POST /api/v1/phones/{serial}/check`: runs the drift check now.
async fn check(
    State(state): State<AppState>,
    ApiPath(serial): ApiPath<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(error) = same_site_only(&headers) {
        return error.into_response();
    }
    match state.monitor().check(&serial).await {
        Ok(report) => Json(report).into_response(),
        Err(error) => ApiError::conflict(error.to_string()).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    fn headers(pairs: &[[&'static str; 2]]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for [name, value] in pairs {
            map.insert(*name, HeaderValue::from_static(value));
        }
        map
    }

    #[test]
    fn only_json_or_the_action_header_from_this_site_passes() {
        for refused in [
            headers(&[]),
            headers(&[["content-type", "application/x-www-form-urlencoded"]]),
            headers(&[["content-type", "text/plain"]]),
            headers(&[
                ["content-type", "application/json"],
                ["sec-fetch-site", "cross-site"],
            ]),
            headers(&[["x-spareline", "1"], ["sec-fetch-site", "cross-site"]]),
            headers(&[["x-spareline", "yes"]]),
        ] {
            let error = same_site_only(&refused).unwrap_err();
            assert_eq!(error.status().as_u16(), 403, "{refused:?}");
            assert_eq!(
                error.message(),
                "send Content-Type: application/json or x-spareline: 1, from this site"
            );
        }
        for allowed in [
            headers(&[["content-type", "application/json"]]),
            headers(&[["x-spareline", "1"], ["sec-fetch-site", "same-origin"]]),
        ] {
            assert!(same_site_only(&allowed).is_ok(), "{allowed:?}");
        }
    }
}
