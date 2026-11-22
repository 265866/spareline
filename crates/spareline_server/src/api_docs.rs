//! The API's `OpenAPI` document and the page that shows it.

/// The `OpenAPI` 3.1 document of every route, as the build this project replaces served it, in
/// its key order. Its `info.version` reads [`VERSION_PLACEHOLDER`] until the running version
/// replaces it.
const DOCUMENT: &str = include_str!("../assets/openapi.json");

/// The text the document holds where the running version goes, exactly once.
const VERSION_PLACEHOLDER: &str = r#""version":"0.0.0""#;

/// The page at `/api/docs`, which loads Scalar's viewer from jsDelivr in the browser and points
/// it at `/api/docs/json`.
pub const DOCS_PAGE: &str = r#"<!doctype html>
<html>
  <head>
    <title>spareline</title>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <style>
      body {
        margin: 0;
      }
    </style>
  </head>
  <body>
    <script id="api-reference" data-url="/api/docs/json"></script>
    <script src="https://cdn.jsdelivr.net/npm/@scalar/api-reference@latest/dist/browser/standalone.min.js" crossorigin></script>
  </body>
</html>
"#;

/// The `OpenAPI` document with `info.version` set to `version`, keeping the document's key
/// order, which the viewer shows routes and fields in.
#[must_use]
pub fn openapi_document(version: &str) -> String {
    let quoted = serde_json::to_string(version).unwrap_or_else(|_| "\"\"".to_owned());
    DOCUMENT
        .trim_end()
        .replacen(VERSION_PLACEHOLDER, &format!(r#""version":{quoted}"#), 1)
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;

    #[test]
    fn the_placeholder_appears_exactly_once() {
        assert_eq!(DOCUMENT.matches(VERSION_PLACEHOLDER).count(), 1);
    }

    #[test]
    fn the_document_keeps_the_running_builds_key_order() {
        let served = openapi_document("0.2.0");
        assert!(
            served.starts_with(r#"{"openapi":"3.1.2","info":{"title":"spareline""#),
            "{}",
            &served[..60]
        );
        let first_path = served.find(r#""/healthz""#).unwrap();
        let second_path = served.find(r#""/api/v1/health""#).unwrap();
        assert!(first_path < second_path, "routes stay in document order");
    }

    #[test]
    fn the_document_names_the_running_version_and_every_route() {
        let document: Value = serde_json::from_str(&openapi_document("0.2.0")).unwrap();
        assert_eq!(document["openapi"], "3.1.2");
        assert_eq!(document["info"]["version"], "0.2.0");
        let paths = document["paths"].as_object().unwrap();
        for path in [
            "/healthz",
            "/api/v1/auth/login",
            "/api/v1/messages",
            "/api/v1/events",
            "/api/v1/phones/{serial}/screen",
        ] {
            assert!(paths.contains_key(path), "{path} is documented");
        }
    }
}
