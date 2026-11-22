//! The API docs, the web UI's assets, and its page for every other path.

use axum::extract::{Request, State};
use axum::http::{HeaderValue, Method, header};
use axum::response::{IntoResponse, Response};
use axum::{Router, routing};

use crate::api_docs;
use crate::api_error::ApiError;
use crate::app_state::AppState;
use crate::http::api_path::ApiPath;

/// The docs and asset routes. The page for every other path is [`fallback`].
pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/docs", routing::get(docs_page))
        .route("/api/docs/json", routing::get(docs_json))
        .route("/assets/", routing::get(no_asset))
        .route("/assets/{*rest}", routing::get(asset))
}

/// `GET /api/docs`: the page that shows the `OpenAPI` document.
async fn docs_page() -> Response {
    (
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/html; charset=utf-8"),
        )],
        api_docs::DOCS_PAGE,
    )
        .into_response()
}

/// `GET /api/docs/json`: the `OpenAPI` document, naming the running version.
async fn docs_json(State(state): State<AppState>) -> Response {
    (
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        )],
        api_docs::openapi_document(state.version()),
    )
        .into_response()
}

/// `GET /assets/<rest>`: a built asset, cached forever.
async fn asset(State(state): State<AppState>, ApiPath(rest): ApiPath<String>) -> Response {
    state.web().asset(&rest).await
}

/// `GET /assets/`: the directory itself, which is no asset.
async fn no_asset(State(state): State<AppState>) -> Response {
    state.web().asset("").await
}

/// Every request no route took: `GET` and `HEAD` get the web UI's page, since it routes by path
/// in the browser (or a JSON 404 under `/api/`), and any other method a JSON 404.
pub(crate) async fn fallback(State(state): State<AppState>, request: Request) -> Response {
    if request.method() == Method::GET || request.method() == Method::HEAD {
        state.web().page(request.uri().path()).await
    } else {
        ApiError::not_found("not found".to_owned()).into_response()
    }
}
