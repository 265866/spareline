//! The API's `OpenAPI` document and the page that shows it.

use serde_json::Value;

/// The `OpenAPI` 3.1 document of every route, as the build this project replaces served it. Its
/// `info.version` is filled in with the running version.
const DOCUMENT: &str = include_str!("../assets/openapi.json");

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

/// The `OpenAPI` document with `info.version` set to `version`, serialized compactly.
///
/// # Panics
///
/// Panics if the document built into the binary isn't JSON, which a test rules out.
#[must_use]
pub fn openapi_document(version: &str) -> String {
    let mut document: Value =
        serde_json::from_str(DOCUMENT).expect("the built-in OpenAPI document is JSON");
    document["info"]["version"] = Value::String(version.to_owned());
    document.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

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
