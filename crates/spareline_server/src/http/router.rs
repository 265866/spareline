//! Every route of the HTTP service, behind the guard, with spareline-old's trailing-slash
//! tolerance.

use axum::extract::Request;
use axum::http::Uri;
use axum::http::uri::PathAndQuery;
use axum::{Router, middleware};

use crate::app_state::AppState;
use crate::http::{
    auth_routes, event_stream, guard, key_routes, message_routes, monitor_routes, number_routes,
    phone_routes, screen_routes, sim_routes, static_routes, status_routes,
};

/// The service's routes. A path with trailing slashes is routed as the path without them, as
/// spareline-old's router ignored them, and every `/api/` request passes the guard first. A
/// method a path has no route for is a 404, as an unknown path is, never a 405. No route sends
/// CORS headers.
pub fn router(state: AppState) -> Router {
    let routes = Router::new()
        .merge(auth_routes::routes())
        .merge(key_routes::routes())
        .merge(status_routes::routes())
        .merge(phone_routes::routes())
        .merge(sim_routes::routes())
        .merge(number_routes::routes())
        .merge(message_routes::routes())
        .merge(event_stream::routes())
        .merge(monitor_routes::routes())
        .merge(screen_routes::routes())
        .merge(static_routes::routes())
        .fallback(static_routes::fallback)
        .method_not_allowed_fallback(static_routes::fallback)
        .layer(middleware::from_fn_with_state(state.clone(), guard::guard))
        .with_state(state);
    Router::new()
        .fallback_service(routes)
        .layer(middleware::map_request(trim_trailing_slashes))
}

/// The request with the trailing slashes of its path removed, except for the root and for
/// asset paths, where `/assets/` names the directory rather than the UI page.
async fn trim_trailing_slashes(mut request: Request) -> Request {
    let path = request.uri().path();
    if path.len() <= 1 || !path.ends_with('/') || path.starts_with("/assets/") {
        return request;
    }
    let trimmed = path.trim_end_matches('/');
    let trimmed = if trimmed.is_empty() { "/" } else { trimmed };
    let path_and_query = match request.uri().query() {
        Some(query) => format!("{trimmed}?{query}"),
        None => trimmed.to_owned(),
    };
    let mut parts = request.uri().clone().into_parts();
    if let Ok(rewritten) = PathAndQuery::try_from(path_and_query) {
        parts.path_and_query = Some(rewritten);
        if let Ok(uri) = Uri::from_parts(parts) {
            *request.uri_mut() = uri;
        }
    }
    request
}
