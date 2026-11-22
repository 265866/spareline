//! The live build's answers to edge-case requests, measured on isolated copies of it, replayed
//! against the router: each must get the same status and, for an error, the same body bytes.

use axum::body::{self, Body};
use axum::http::{Method, Request};
use serde::Deserialize;

use crate::test_app::{self, TestApp};

/// The requests and the live build's answers, in the order they were sent. A path names the
/// seeded phone, SIM and number as `{serial}`, `{sim}` and `{number}`.
const FIXTURES: &str = include_str!("../fixtures/http/live-fixtures.json");

/// The body the fixture file stands in for the 3 MiB request, which the test writes itself.
const BIG_BODY: &str = "<a JSON object with a 3 MiB string>";

/// One request and the live build's answer.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Fixture {
    /// What the request checks.
    name: String,
    /// The method.
    method: String,
    /// The path and query, with placeholders.
    path: String,
    /// The headers, each `Name: value`.
    headers: Vec<String>,
    /// Whether the request carries a signed-in session.
    signed_in: bool,
    /// The body, if any.
    body: Option<String>,
    /// The live status, in digits.
    status: String,
    /// The live body, for an error.
    response: Option<String>,
}

impl Fixture {
    /// The request as curl sent it to the live build: `--data-binary` gives a body without a
    /// `Content-Type` of its own the form type.
    fn request(&self, session: &str) -> Request<Body> {
        let path = self
            .path
            .replace("{serial}", "SPARELINE00001")
            .replace("{sim}", "1")
            .replace("{number}", "1");
        let method: Method = self.method.parse().expect("a method");
        let mut request = Request::builder().method(method).uri(path);
        let mut has_content_type = false;
        for header in &self.headers {
            let (name, value) = header.split_once(": ").expect("a `Name: value` header");
            has_content_type |= name.eq_ignore_ascii_case("content-type");
            request = request.header(name, value);
        }
        if self.signed_in {
            request = request.header("cookie", session);
        }
        let body = match self.body.as_deref() {
            None => Body::empty(),
            Some(text) => {
                if !has_content_type {
                    request = request.header("content-type", "application/x-www-form-urlencoded");
                }
                if text == BIG_BODY {
                    Body::from(format!("{{\"note\":\"{}\"}}", "x".repeat(3 * 1024 * 1024)))
                } else {
                    Body::from(text.to_owned())
                }
            }
        };
        request.body(body).expect("the request is valid")
    }
}

#[test]
fn every_edge_request_gets_the_live_builds_status_and_error_body() {
    use tower::ServiceExt as _;

    test_app::run(async {
        let fixtures: Vec<Fixture> = serde_json::from_str(FIXTURES).expect("the fixtures are JSON");
        assert_eq!(fixtures.len(), 138, "every fixture is read");
        let app = TestApp::start();
        app.add_phone();
        let session = app.alice().await;
        let mut mismatches = Vec::new();
        for fixture in &fixtures {
            let response = app
                .router
                .clone()
                .oneshot(fixture.request(&session))
                .await
                .expect("the router answers");
            let status = response.status().as_u16().to_string();
            let content_type = response
                .headers()
                .get("content-type")
                .map(|value| value.to_str().expect("a text header").to_owned());
            if response.status().is_success() && fixture.response.is_none() {
                if status != fixture.status {
                    mismatches.push(format!(
                        "{}: status {status}, live {}",
                        fixture.name, fixture.status
                    ));
                }
                continue;
            }
            let bytes = body::to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("the body is read");
            let text = String::from_utf8_lossy(&bytes);
            if status != fixture.status {
                mismatches.push(format!(
                    "{}: status {status}, live {}: {text}",
                    fixture.name, fixture.status
                ));
                continue;
            }
            let Some(expected) = fixture.response.as_deref() else {
                continue;
            };
            if text != expected {
                mismatches.push(format!(
                    "{}: body {text}\n    live {expected}",
                    fixture.name
                ));
            } else if expected.starts_with("{\"error\"")
                && content_type.as_deref() != Some("application/json")
            {
                mismatches.push(format!(
                    "{}: content type {content_type:?} for a JSON error",
                    fixture.name
                ));
            }
        }
        assert!(
            mismatches.is_empty(),
            "{} of {} differ from the live build:\n{}",
            mismatches.len(),
            fixtures.len(),
            mismatches.join("\n")
        );
    });
}
