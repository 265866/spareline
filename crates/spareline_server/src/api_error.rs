//! The error body every API route answers with.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use thiserror::Error;

/// A failed API request: a status code and the `{"error": "..."}` body clients show.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("{status}: {message}")]
#[must_use]
pub struct ApiError {
    status: StatusCode,
    message: String,
}

/// The JSON body of an [`ApiError`].
#[derive(Serialize)]
struct ErrorBody<'message> {
    error: &'message str,
}

impl ApiError {
    /// An error with `status` and `message`.
    pub fn new(status: StatusCode, message: String) -> Self {
        Self { status, message }
    }

    /// 400, for a request the route can't act on.
    pub fn bad_request(message: String) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }

    /// 401, for a request without a valid session or key.
    pub fn unauthorized(message: String) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, message)
    }

    /// 403, for a request the session or key may not make.
    pub fn forbidden(message: String) -> Self {
        Self::new(StatusCode::FORBIDDEN, message)
    }

    /// 404, for something that doesn't exist.
    pub fn not_found(message: String) -> Self {
        Self::new(StatusCode::NOT_FOUND, message)
    }

    /// 409, for a request that conflicts with the current state.
    pub fn conflict(message: String) -> Self {
        Self::new(StatusCode::CONFLICT, message)
    }

    /// 415, for a body that isn't JSON where JSON is required.
    pub fn unsupported_media_type() -> Self {
        Self::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "send Content-Type: application/json".to_owned(),
        )
    }

    /// 422, for a query or body that fails validation, such as
    /// `limit: expected an integer from 1 to 500, got "0"`.
    pub fn invalid(message: String) -> Self {
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, message)
    }

    /// The status code.
    #[must_use]
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// The message clients show.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = ErrorBody {
            error: &self.message,
        };
        (self.status, Json(body)).into_response()
    }
}
