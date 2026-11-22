//! The built web UI: its hashed assets and the single page every other path answers with.

use std::io;
use std::path::{Component, Path, PathBuf};

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use tokio::fs;

use crate::api_error::ApiError;

/// Assets carry a content hash in their name, so a browser may keep them forever.
const ASSET_CACHE: &str = "public, max-age=31536000, immutable";

/// The web UI's files on disk, under the directory `SPARELINE_WEB_DIR` names.
#[derive(Clone, Debug)]
pub struct WebFiles {
    root: PathBuf,
}

impl WebFiles {
    /// The UI built into `root`, which holds `index.html` and `assets/`.
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Answers `GET /assets/<rest>`: the file, cached forever, or a plain-text 404 for anything
    /// missing or outside `assets/`.
    pub async fn asset(&self, rest: &str) -> Response {
        let Some(path) = self.asset_path(rest) else {
            return not_found_text();
        };
        match read_file(&path).await {
            Ok(Some(bytes)) => {
                let content_type = content_type_of(&path);
                (
                    [
                        (header::CONTENT_TYPE, content_type),
                        (header::CACHE_CONTROL, HeaderValue::from_static(ASSET_CACHE)),
                    ],
                    bytes,
                )
                    .into_response()
            }
            Ok(None) | Err(_) => not_found_text(),
        }
    }

    /// Answers `GET` on any other path: `index.html`, uncached, since the UI routes by path in the
    /// browser. A path under `api/` that no route took is a JSON 404, and a missing build is a
    /// plain-text 503.
    pub async fn page(&self, path: &str) -> Response {
        if path.trim_start_matches('/').starts_with("api/") {
            return ApiError::not_found("not found".to_owned()).into_response();
        }
        match read_file(&self.root.join("index.html")).await {
            Ok(Some(bytes)) => (
                [
                    (
                        header::CONTENT_TYPE,
                        HeaderValue::from_static("text/html; charset=utf-8"),
                    ),
                    (header::CACHE_CONTROL, HeaderValue::from_static("no-cache")),
                ],
                bytes,
            )
                .into_response(),
            Ok(None) | Err(_) => (
                StatusCode::SERVICE_UNAVAILABLE,
                [(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("text/plain; charset=utf-8"),
                )],
                "The web UI is not built. Run `just web-build`.",
            )
                .into_response(),
        }
    }

    /// The file `rest` names under `assets/`, or `None` when it would leave that directory.
    fn asset_path(&self, rest: &str) -> Option<PathBuf> {
        let relative = Path::new(rest);
        if rest.is_empty() || rest.contains('\0') || rest.contains('\\') {
            return None;
        }
        let only_names = relative
            .components()
            .all(|component| matches!(component, Component::Normal(_)));
        only_names.then(|| self.root.join("assets").join(relative))
    }
}

/// The bytes of the regular file at `path`, or `None` when there is none.
async fn read_file(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match fs::metadata(path).await {
        Ok(metadata) if metadata.is_file() => fs::read(path).await.map(Some),
        Ok(_) => Ok(None),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// The `Content-Type` of a UI file, from its extension, with UTF-8 named for text.
fn content_type_of(path: &Path) -> HeaderValue {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("");
    let value = match extension {
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "html" => "text/html; charset=utf-8",
        "json" | "map" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    };
    HeaderValue::from_static(value)
}

fn not_found_text() -> Response {
    (
        StatusCode::NOT_FOUND,
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        )],
        "not found",
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::{env, fs as std_fs, process};

    use axum::body::{self, Body};
    use axum::http::HeaderName;
    use tokio::runtime::Builder;

    use super::*;

    fn block_on<F: Future>(future: F) -> F::Output {
        Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(future)
    }

    /// Tells apart the directories of tests that run at once in this process.
    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct Built {
        directory: PathBuf,
        files: WebFiles,
    }

    impl Drop for Built {
        fn drop(&mut self) {
            std_fs::remove_dir_all(&self.directory).unwrap();
        }
    }

    #[derive(Clone, Copy)]
    enum Index {
        Built,
        Missing,
    }

    fn built(index: Index) -> Built {
        let directory = env::temp_dir().join(format!(
            "spareline-web-files-{}-{}",
            process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        let web = directory.join("web");
        std_fs::create_dir_all(web.join("assets/nested")).unwrap();
        if matches!(index, Index::Built) {
            std_fs::write(
                web.join("index.html"),
                "<!doctype html><title>spareline</title>",
            )
            .unwrap();
        }
        std_fs::write(web.join("assets/index-abc123.js"), "console.log(1)").unwrap();
        std_fs::write(web.join("assets/nested/x.css"), "a{}").unwrap();
        std_fs::write(directory.join("secret.txt"), "not served").unwrap();
        Built {
            files: WebFiles::new(web),
            directory,
        }
    }

    async fn text(response: Response) -> String {
        let bytes = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    fn header_of(response: &Response, name: HeaderName) -> String {
        response.headers()[name].to_str().unwrap().to_owned()
    }

    #[test]
    fn serves_an_asset_cached_forever() {
        block_on(async {
            let built = built(Index::Built);
            let response = built.files.asset("index-abc123.js").await;
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(header_of(&response, header::CACHE_CONTROL), ASSET_CACHE);
            assert_eq!(
                header_of(&response, header::CONTENT_TYPE),
                "text/javascript; charset=utf-8"
            );
            assert_eq!(text(response).await, "console.log(1)");
            let nested = built.files.asset("nested/x.css").await;
            assert_eq!(
                header_of(&nested, header::CONTENT_TYPE),
                "text/css; charset=utf-8"
            );
        });
    }

    #[test]
    fn refuses_paths_that_leave_the_assets() {
        block_on(async {
            let built = built(Index::Built);
            for rest in [
                "../../secret.txt",
                "/etc/passwd",
                "nested/../../index.html",
                "",
                "a\\b",
                "nested",
            ] {
                let response = built.files.asset(rest).await;
                assert_eq!(response.status(), StatusCode::NOT_FOUND, "{rest:?}");
                assert_eq!(text(response).await, "not found");
            }
        });
    }

    #[test]
    fn answers_every_page_with_the_index() {
        block_on(async {
            let built = built(Index::Built);
            for path in ["/", "/numbers", "/numbers/+15125550101", "/apix", "/api"] {
                let response = built.files.page(path).await;
                assert_eq!(response.status(), StatusCode::OK, "{path}");
                assert_eq!(header_of(&response, header::CACHE_CONTROL), "no-cache");
                assert_eq!(
                    header_of(&response, header::CONTENT_TYPE),
                    "text/html; charset=utf-8"
                );
                assert!(text(response).await.contains("<title>spareline</title>"));
            }
        });
    }

    #[test]
    fn an_unknown_api_path_is_a_json_404() {
        block_on(async {
            let built = built(Index::Built);
            let response = built.files.page("/api/v1/nope").await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
            assert_eq!(text(response).await, r#"{"error":"not found"}"#);
        });
    }

    #[test]
    fn a_missing_build_is_a_503() {
        block_on(async {
            let built = built(Index::Missing);
            let response = built.files.page("/").await;
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(
                text(response).await,
                "The web UI is not built. Run `just web-build`."
            );
        });
    }

    #[test]
    fn errors_answer_with_a_json_error_body() {
        block_on(async {
            let response = ApiError::conflict("taken".to_owned()).into_response();
            assert_eq!(response.status(), StatusCode::CONFLICT);
            assert_eq!(
                header_of(&response, header::CONTENT_TYPE),
                "application/json"
            );
            let body: Body = response.into_body();
            let bytes = body::to_bytes(body, usize::MAX).await.unwrap();
            assert_eq!(bytes.as_ref(), br#"{"error":"taken"}"#);
        });
    }
}
