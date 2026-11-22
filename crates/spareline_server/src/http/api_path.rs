//! Path parameters that are refused with the API's JSON error body.

use axum::extract::rejection::PathRejection;
use axum::extract::{FromRequestParts, Path};
use axum::http::request::Parts;
use serde::de::DeserializeOwned;

use crate::api_error::ApiError;

/// The route's path parameters as axum's [`Path`] reads them, but a refusal, such as a
/// parameter that isn't UTF-8 once decoded, answers `{"error": <axum's message>}` with the JSON
/// content type, as the live build did, rather than axum's plain text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ApiPath<T>(pub(crate) T);

impl<S: Send + Sync, T: DeserializeOwned + Send> FromRequestParts<S> for ApiPath<T> {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, ApiError> {
        let extracted: Result<Path<T>, PathRejection> =
            Path::from_request_parts(parts, state).await;
        extracted
            .map(|path| Self(path.0))
            .map_err(|rejection| ApiError::new(rejection.status(), rejection.body_text()))
    }
}
