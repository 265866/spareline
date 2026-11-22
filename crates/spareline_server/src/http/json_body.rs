//! Request bodies: the JSON-only rule, and typed bodies read as the live build read them.

use axum::body::Bytes;
use axum::extract::{FromRequest, Request};
use axum::http::{HeaderMap, StatusCode, header};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};
use serde_json::Number;
use serde_json::error::Category;
use spareline_core::Timestamp;
use spareline_db::FieldPatch;

use crate::api_error::ApiError;
use crate::http::{js_text, query_params};

/// The largest whole number JavaScript's numbers hold exactly, `Number.MAX_SAFE_INTEGER`.
pub(crate) const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

/// Whether the request says its body is JSON: a `Content-Type` that starts with
/// `application/json`. A page on another site can post a form or text, but not JSON without a
/// CORS preflight, which this server never grants.
pub(crate) fn is_json(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .is_some_and(|value| value.as_bytes().starts_with(b"application/json"))
}

/// Refuses a request whose body isn't declared JSON.
///
/// # Errors
///
/// A 415 `send Content-Type: application/json`.
pub(crate) fn require_json(headers: &HeaderMap) -> Result<(), ApiError> {
    if is_json(headers) {
        Ok(())
    } else {
        Err(ApiError::unsupported_media_type())
    }
}

/// A JSON request body read into `T`, as the live build's axum `Json` extractor read it. As an
/// extractor it first refuses a body not declared JSON (415), then one over axum's body limit
/// (413), then reads the body with [`JsonBody::parse`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct JsonBody<T>(pub(crate) T);

impl<T: DeserializeOwned> JsonBody<T> {
    /// Reads `bytes` as one JSON value of type `T`.
    ///
    /// # Errors
    ///
    /// An empty body is a 422 with `serde_json`'s message. Otherwise malformed or truncated JSON,
    /// and text after the value, are a 400, and JSON of the wrong shape a 422. The message is
    /// `serde_json`'s, after the path to the value that failed, such as
    /// `costCents: expected a whole number of cents from 0 to 9007199254740991, got -1 at line 1
    /// column 16`, with no path for the body itself and `?` where the path is unknown.
    pub(crate) fn parse(bytes: &[u8]) -> Result<Self, ApiError> {
        let mut deserializer = serde_json::Deserializer::from_slice(bytes);
        let value = serde_path_to_error::deserialize(&mut deserializer).map_err(|error| {
            let status = if bytes.is_empty() {
                StatusCode::UNPROCESSABLE_ENTITY
            } else {
                status_of(error.inner())
            };
            let message = if error.path().iter().next().is_none() {
                error.inner().to_string()
            } else {
                format!("{}: {}", error.path(), error.inner())
            };
            ApiError::new(status, message)
        })?;
        deserializer
            .end()
            .map_err(|error| ApiError::new(status_of(&error), error.to_string()))?;
        Ok(Self(value))
    }
}

impl<S: Send + Sync, T: DeserializeOwned> FromRequest<S> for JsonBody<T> {
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> Result<Self, ApiError> {
        require_json(request.headers())?;
        let bytes = Bytes::from_request(request, state)
            .await
            .map_err(|rejection| ApiError::new(rejection.status(), rejection.body_text()))?;
        Self::parse(&bytes)
    }
}

/// The status of a body `serde_json` refused: 422 for JSON of the wrong shape, 400 for anything
/// that isn't JSON.
fn status_of(error: &serde_json::Error) -> StatusCode {
    match error.classify() {
        Category::Data => StatusCode::UNPROCESSABLE_ENTITY,
        Category::Io | Category::Syntax | Category::Eof => StatusCode::BAD_REQUEST,
    }
}

/// Text of `MIN` to `MAX` characters, counted in UTF-16 code units as JavaScript counts a
/// string's length.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Text<const MIN: usize, const MAX: usize>(pub(crate) String);

impl<'de, const MIN: usize, const MAX: usize> Deserialize<'de> for Text<MIN, MAX> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;

        let text = String::deserialize(deserializer)?;
        let length = js_text::length(&text);
        if length > MAX {
            return Err(D::Error::custom(format!(
                "expected a string of at most {MAX} characters, got {length}"
            )));
        }
        if length < MIN {
            return Err(D::Error::custom(format!(
                "expected a string of at least {MIN} characters, got {length}"
            )));
        }
        Ok(Self(text))
    }
}

/// A whole number of cents from 0 to [`MAX_SAFE_INTEGER`], written as any JSON number that is
/// whole, such as `100`, `100.0` or `1e2`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Cents(pub(crate) i64);

impl<'de> Deserialize<'de> for Cents {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;

        let number = Number::deserialize(deserializer)?;
        let whole = number.as_i64().or_else(|| {
            number
                .as_f64()
                .and_then(|float| js_text::whole_number(&float.to_string()))
        });
        match whole {
            Some(cents) if (0..=MAX_SAFE_INTEGER).contains(&cents) => Ok(Self(cents)),
            Some(_) | None => Err(D::Error::custom(format!(
                "expected a whole number of cents from 0 to {MAX_SAFE_INTEGER}, got {number}"
            ))),
        }
    }
}

/// A calendar date written `YYYY-MM-DD`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Date(pub(crate) String);

impl<'de> Deserialize<'de> for Date {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;

        let text = String::deserialize(deserializer)?;
        if Timestamp::parse_date(&text).is_none() {
            return Err(D::Error::custom(format!(
                "expected a date written YYYY-MM-DD, got {}",
                query_params::quoted(&text)
            )));
        }
        Ok(Self(text))
    }
}

/// Reads a field that may be `null`, which clears it. Use it with `#[serde(default)]`, so a
/// field left out is [`FieldPatch::Absent`].
///
/// # Errors
///
/// What `T` refuses.
pub(crate) fn nullable<'de, D, T>(deserializer: D) -> Result<FieldPatch<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(match Option::<T>::deserialize(deserializer)? {
        Some(value) => FieldPatch::Value(value),
        None => FieldPatch::Null,
    })
}

/// Reads a field that may not be `null`. Anything else goes to `T`, so an enum given a number
/// gets `serde_json`'s `expected value`.
///
/// # Errors
///
/// `expected a value, got null`, and what `T` refuses.
pub(crate) fn non_null<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    use serde::de::Error as _;

    Option::<T>::deserialize(deserializer)?
        .ok_or_else(|| D::Error::custom("expected a value, got null"))
}

/// Reads a field that may be left out but not `null`, as [`non_null`]. Use it with
/// `#[serde(default)]`, so a field left out is `None`.
///
/// # Errors
///
/// As [`non_null`].
pub(crate) fn optional_non_null<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    non_null(deserializer).map(Some)
}

/// `patch` with its value, when it has one, turned by `convert`.
pub(crate) fn map_patch<T, U>(patch: FieldPatch<T>, convert: fn(T) -> U) -> FieldPatch<U> {
    match patch {
        FieldPatch::Absent => FieldPatch::Absent,
        FieldPatch::Null => FieldPatch::Null,
        FieldPatch::Value(value) => FieldPatch::Value(convert(value)),
    }
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;
    use serde::Deserialize;

    use super::*;

    /// A body with each kind of field.
    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Sample {
        #[serde(default, deserialize_with = "nullable")]
        label: FieldPatch<Text<0, 4>>,
        #[serde(default, deserialize_with = "nullable")]
        cost_cents: FieldPatch<Cents>,
        #[serde(default, deserialize_with = "optional_non_null")]
        day: Option<Date>,
        #[serde(default, deserialize_with = "optional_non_null")]
        name: Option<Text<1, 64>>,
    }

    /// The status and message a refused body gets.
    #[derive(Debug, PartialEq, Eq)]
    struct Refusal {
        status: u16,
        message: String,
    }

    fn refusal(json: &str) -> Refusal {
        let error = JsonBody::<Sample>::parse(json.as_bytes()).unwrap_err();
        Refusal {
            status: error.status().as_u16(),
            message: error.message().to_owned(),
        }
    }

    fn refused(status: u16, message: &str) -> Refusal {
        Refusal {
            status,
            message: message.to_owned(),
        }
    }

    #[test]
    fn only_a_json_content_type_counts() {
        let mut headers = HeaderMap::new();
        assert!(!is_json(&headers));
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json; charset=utf-8"),
        );
        assert!(is_json(&headers));
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/x-www-form-urlencoded"),
        );
        assert_eq!(require_json(&headers).unwrap_err().status().as_u16(), 415);
    }

    #[test]
    fn reads_absent_null_and_set_fields() {
        let JsonBody(sample) = JsonBody::<Sample>::parse(
            br#"{"label":null,"costCents":1e2,"day":"2024-02-29","name":"x"}"#,
        )
        .unwrap();
        assert_eq!(sample.label, FieldPatch::Null);
        assert_eq!(sample.cost_cents, FieldPatch::Value(Cents(100)));
        assert_eq!(sample.day, Some(Date("2024-02-29".to_owned())));
        assert_eq!(sample.name, Some(Text("x".to_owned())));
        let JsonBody(empty) = JsonBody::<Sample>::parse(b"[]").unwrap();
        assert_eq!(
            empty.label,
            FieldPatch::Absent,
            "an array reads as the fields in order"
        );
        assert_eq!(empty.day, None);
    }

    #[test]
    fn malformed_json_is_a_400_and_the_wrong_shape_a_422() {
        assert_eq!(
            refusal(""),
            refused(422, "EOF while parsing a value at line 1 column 0")
        );
        assert_eq!(
            refusal("{"),
            refused(400, "?: EOF while parsing an object at line 1 column 1")
        );
        assert_eq!(
            refusal("{} x"),
            refused(400, "trailing characters at line 1 column 4")
        );
        assert_eq!(
            refusal("null"),
            refused(
                422,
                "invalid type: null, expected struct Sample at line 1 column 4"
            )
        );
        assert_eq!(
            refusal(r#"{"label":"a","label":"b"}"#),
            refused(422, "duplicate field `label` at line 1 column 20")
        );
    }

    #[test]
    fn field_checks_name_the_field_and_the_limit() {
        assert_eq!(
            refusal("{\"label\":\"\u{1f600}\u{1f600}x\"}"),
            refused(
                422,
                "label: expected a string of at most 4 characters, got 5 at line 1 column 21"
            )
        );
        assert_eq!(
            refusal(r#"{"name":""}"#),
            refused(
                422,
                "name: expected a string of at least 1 characters, got 0 at line 1 column 11"
            )
        );
        assert_eq!(
            refusal(r#"{"costCents":1.5}"#).message,
            "costCents: expected a whole number of cents from 0 to 9007199254740991, got 1.5 \
             at line 1 column 17"
        );
        assert_eq!(
            refusal(r#"{"costCents":9007199254740992}"#).message,
            "costCents: expected a whole number of cents from 0 to 9007199254740991, got \
             9007199254740992 at line 1 column 30"
        );
        assert_eq!(
            refusal(r#"{"day":"2026-02-30"}"#).message,
            "day: expected a date written YYYY-MM-DD, got \"2026-02-30\" at line 1 column 20"
        );
        assert_eq!(
            refusal(r#"{"day":null}"#),
            refused(422, "day: expected a value, got null at line 1 column 12")
        );
        assert_eq!(
            refusal(r#"{"nope":1}"#).message,
            "nope: unknown field `nope`, expected one of `label`, `costCents`, `day`, `name` at \
             line 1 column 7"
        );
    }
}
