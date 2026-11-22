//! Texts and their attachments: pages, long polls and the attachment files.

use std::time::Duration;
use std::{io, pin};

use axum::extract::{RawQuery, State};
use axum::http::{HeaderName, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json, Router, routing};
use futures_util::future;
use spareline_db::{DatabaseError, Inventory, MessagePage, MessageScope, Paging, Principal};
use tokio::sync::broadcast::error::RecvError;
use tokio::{fs, time};

use crate::api_error::ApiError;
use crate::app_state::AppState;
use crate::http::api_path::ApiPath;
use crate::http::js_text;
use crate::http::json_body::MAX_SAFE_INTEGER;
use crate::http::query_params::{IntRange, QueryParams};
use crate::http::revocation::Revocation;

/// How many texts a page holds unless `limit` says otherwise.
const DEFAULT_LIMIT: i64 = 50;

/// The routes for texts and attachments.
pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/numbers/{key}/messages",
            routing::get(number_messages),
        )
        .route("/api/v1/messages", routing::get(all_messages))
        .route("/api/v1/attachments/{id}", routing::get(attachment))
}

/// The paging parameters both text routes take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PageQuery {
    limit: i64,
    before: Option<i64>,
    after: Option<i64>,
    wait: i64,
}

impl PageQuery {
    /// Reads `limit` (1-500), `before` and `after` (0 to [`MAX_SAFE_INTEGER`]) and `wait`
    /// (0-30 s).
    fn parse(query: &QueryParams) -> Result<Self, ApiError> {
        let limit = query.integer("limit", IntRange::between(1, 500))?;
        let before = query.integer("before", IntRange::between(0, MAX_SAFE_INTEGER))?;
        let after = query.integer("after", IntRange::between(0, MAX_SAFE_INTEGER))?;
        let wait = query.integer("wait", IntRange::between(0, 30))?;
        Ok(Self {
            limit: limit.unwrap_or(DEFAULT_LIMIT),
            before,
            after,
            wait: wait.unwrap_or(0),
        })
    }
}

/// `GET /api/v1/numbers/{key}/messages`: the texts to one number, by id, E.164 or digits.
async fn number_messages(
    State(state): State<AppState>,
    principal: Option<Extension<Principal>>,
    ApiPath(key): ApiPath<String>,
    RawQuery(raw): RawQuery,
) -> Response {
    let query = QueryParams::parse(raw.as_deref());
    let paging = match PageQuery::parse(&query) {
        Ok(paging) => paging,
        Err(error) => return error.into_response(),
    };
    let lookup = key.clone();
    let number_id = match state
        .db()
        .call(move |db| find_number_id(&db.inventory(), &lookup))
        .await
    {
        Ok(Some(id)) => id,
        Ok(None) => return ApiError::not_found(format!("no number {key}")).into_response(),
        Err(error) => return ApiError::internal(state.log(), &error).into_response(),
    };
    let scope = MessageScope {
        number_id: Some(number_id),
        search: None,
    };
    respond(message_page(&state, principal, scope, paging).await)
}

/// `GET /api/v1/messages`: the texts to every number, optionally searched with `q`.
async fn all_messages(
    State(state): State<AppState>,
    principal: Option<Extension<Principal>>,
    RawQuery(raw): RawQuery,
) -> Response {
    let query = QueryParams::parse(raw.as_deref());
    let parsed = PageQuery::parse(&query).and_then(|paging| {
        let search = query
            .text("q", 200)?
            .map(|q| js_text::trim(q).to_owned())
            .filter(|q| !q.is_empty());
        Ok(Search { paging, search })
    });
    let Search { paging, search } = match parsed {
        Ok(search) => search,
        Err(error) => return error.into_response(),
    };
    let scope = MessageScope {
        number_id: None,
        search,
    };
    respond(message_page(&state, principal, scope, paging).await)
}

/// The query of `GET /api/v1/messages`, checked.
struct Search {
    paging: PageQuery,
    search: Option<String>,
}

fn respond(page: Result<MessagePage, ApiError>) -> Response {
    match page {
        Ok(page) => Json(page).into_response(),
        Err(error) => error.into_response(),
    }
}

/// The id of the number `key` names, as spareline-old's `findNumberId` read it: up to 9 digits
/// are an id; `+` and digits are E.164; otherwise a US number of 10 digits, or 11 starting with
/// 1, or else 10 to 15 digits with `+` put in front.
fn find_number_id(inventory: &Inventory<'_>, key: &str) -> Result<Option<i64>, DatabaseError> {
    let trimmed = js_text::trim(key);
    if (1..=9).contains(&trimmed.len()) && trimmed.bytes().all(|byte| byte.is_ascii_digit()) {
        return inventory.number_id_of(trimmed);
    }
    let e164 = if trimmed.starts_with('+') {
        spareline_core::to_e164(trimmed, None)
    } else {
        spareline_core::to_e164(trimmed, Some("us")).or_else(|| {
            let digits = (10..=15).contains(&trimmed.len())
                && trimmed.bytes().all(|byte| byte.is_ascii_digit());
            digits.then(|| format!("+{trimmed}"))
        })
    };
    match e164 {
        Some(e164) => inventory.number_id_of(&e164),
        None => Ok(None),
    }
}

/// One page of texts: newest first, optionally before a seq, or oldest first after a seq,
/// waiting up to `wait` seconds for the first new one. A waiting request re-reads with its own
/// filter on every published text, so it answers exactly what a fresh request would, and
/// answers an empty page when the wait runs out, its principal is signed out, or the service
/// stops.
async fn message_page(
    state: &AppState,
    principal: Option<Extension<Principal>>,
    scope: MessageScope,
    query: PageQuery,
) -> Result<MessagePage, ApiError> {
    if query.before.is_some() && query.after.is_some() {
        return Err(ApiError::bad_request(
            "use before or after, not both".to_owned(),
        ));
    }
    let Some(after) = query.after else {
        let paging = Paging::Newest {
            before: query.before,
            limit: query.limit,
        };
        return read_page(state, scope, paging).await;
    };
    let paging = Paging::After {
        after,
        limit: query.limit,
    };
    let mut texts = state.buses().texts();
    let first = read_page(state, scope.clone(), paging).await?;
    let wait = u64::try_from(query.wait).unwrap_or(0);
    if !first.messages.is_empty() || wait == 0 {
        return Ok(first);
    }
    let empty = || MessagePage {
        messages: Vec::new(),
        cursor: after,
    };
    let arrived = async {
        loop {
            match texts.recv().await {
                Ok(_) | Err(RecvError::Lagged(_)) => {
                    let page = read_page(state, scope.clone(), paging).await?;
                    if !page.messages.is_empty() {
                        return Ok(page);
                    }
                }
                Err(RecvError::Closed) => return Ok(empty()),
            }
        }
    };
    let revoked = async {
        match principal.map(|found| found.0) {
            Some(principal) => Revocation::watch(state, principal).revoked().await,
            None => future::pending().await,
        }
    };
    let revoked = pin::pin!(revoked);
    let stopping = pin::pin!(state.stopping());
    let ended = future::select(revoked, stopping);
    let bounded = time::timeout(Duration::from_secs(wait), arrived);
    match future::select(pin::pin!(bounded), ended).await {
        future::Either::Left((Ok(page), _)) => page,
        future::Either::Left((Err(_), _)) | future::Either::Right(_) => Ok(empty()),
    }
}

async fn read_page(
    state: &AppState,
    scope: MessageScope,
    paging: Paging,
) -> Result<MessagePage, ApiError> {
    state
        .db()
        .call(move |db| db.messages().page(&scope, paging))
        .await
        .map_err(|error| ApiError::internal(state.log(), &error))
}

/// `GET /api/v1/attachments/{id}`: an MMS part's bytes, with its content type. Only raster
/// images show inline; anything else, such as HTML or SVG, downloads, and is sandboxed, so a
/// hostile part can't run script on this origin.
async fn attachment(State(state): State<AppState>, ApiPath(raw): ApiPath<String>) -> Response {
    let id = match IntRange::at_least(1).check("id", &raw) {
        Ok(id) => id,
        Err(error) => return error.into_response(),
    };
    let found = match state
        .db()
        .call(move |db| db.messages().attachment(id))
        .await
    {
        Ok(found) => found,
        Err(error) => return ApiError::internal(state.log(), &error).into_response(),
    };
    let missing = || ApiError::not_found(format!("no attachment {id}")).into_response();
    let Some(file) = found else {
        return missing();
    };
    let Some(path) = state.attachments().path(&file.sha256) else {
        return missing();
    };
    let bytes = match fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return missing(),
        Err(error) => return ApiError::internal(state.log(), &error).into_response(),
    };
    let disposition = if is_inline_image(&file.content_type) {
        "inline"
    } else {
        "attachment"
    };
    let name = safe_file_name(
        file.filename
            .as_deref()
            .unwrap_or(&format!("attachment-{id}")),
    );
    let content_type = HeaderValue::from_str(&file.content_type)
        .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"));
    let disposition = HeaderValue::from_str(&format!("{disposition}; filename=\"{name}\""))
        .unwrap_or_else(|_| HeaderValue::from_static("attachment"));
    let headers = [
        (header::CONTENT_TYPE, content_type),
        (header::CONTENT_DISPOSITION, disposition),
        (
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, max-age=31536000, immutable"),
        ),
        (
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ),
        (
            HeaderName::from_static("content-security-policy"),
            HeaderValue::from_static("sandbox; default-src 'none'"),
        ),
    ];
    (headers, bytes).into_response()
}

/// Whether `content_type` is a raster image a browser may show inline:
/// `image/(png|jpeg|gif|webp|bmp|heic|heif)`, exactly.
fn is_inline_image(content_type: &str) -> bool {
    matches!(
        content_type.strip_prefix("image/"),
        Some("png" | "jpeg" | "gif" | "webp" | "bmp" | "heic" | "heif")
    )
}

/// `name` with every character but ASCII letters, digits, `_`, `.` and `-` replaced by `_`,
/// once per UTF-16 code unit, as JavaScript's `replace(/[^\w.-]/g, "_")` does.
fn safe_file_name(name: &str) -> String {
    let mut safe = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') {
            safe.push(c);
        } else {
            for _ in 0..c.len_utf16() {
                safe.push('_');
            }
        }
    }
    safe
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_keep_only_safe_characters() {
        assert_eq!(safe_file_name("a b.png"), "a_b.png");
        assert_eq!(safe_file_name("x\"y;z"), "x_y_z");
        assert_eq!(safe_file_name("r\u{e9}sum\u{e9}.pdf"), "r_sum_.pdf");
        assert_eq!(
            safe_file_name("\u{1f600}.jpg"),
            "__.jpg",
            "JavaScript replaces each half of a surrogate pair"
        );
    }

    #[test]
    fn only_raster_images_show_inline() {
        for inline in ["image/png", "image/jpeg", "image/heif"] {
            assert!(is_inline_image(inline), "{inline}");
        }
        for download in ["image/svg+xml", "text/html", "image/png; x=1", "IMAGE/PNG"] {
            assert!(!is_inline_image(download), "{download}");
        }
    }
}
