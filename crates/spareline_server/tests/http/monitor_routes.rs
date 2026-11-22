//! Set up phone, the drift check and the status's policy part, ported from spareline-old's
//! `src/api/monitor.test.ts`. The policy's own behavior is the monitor's tests' business; these
//! check what the routes add: the same-site rule, the 409s, and the status view.

use axum::http::StatusCode;
use serde_json::{Value, json};
use spareline_core::monitor::Policy;
use spareline_server::monitor::MonitorSettings;

use crate::test_app::{self, Monitoring, TestApp};

const B30_PROFILE: &str = include_str!("../fixtures/nuu-b30-no-data.json");

fn b30_settings() -> MonitorSettings {
    MonitorSettings {
        policy: Some(
            Policy::load("nuu-b30-no-data.json", B30_PROFILE).expect("the example profile loads"),
        ),
        ..MonitorSettings::default()
    }
}

#[test]
fn the_status_carries_the_profile_and_the_threshold() {
    test_app::run(async {
        let app = TestApp::start_with(b30_settings());
        let session = app.alice().await;
        let status = app
            .call("GET", "/api/v1/status", &[["cookie", &session]], None)
            .await
            .json();
        assert_eq!(status["dataThresholdBytes"], 1_000_000);
        assert_eq!(status["profile"]["name"], "nuu-b30-no-data");
        assert_eq!(status["profile"]["removesApps"], true);
        let set_up = status["profile"]["setUp"]
            .as_array()
            .expect("setUp")
            .clone();
        assert!(
            set_up.contains(&json!("Unneeded apps removed")),
            "{set_up:?}"
        );
        assert!(
            set_up.contains(&json!("Google Messages is the default SMS app")),
            "{set_up:?}"
        );
        assert_eq!(status["alerts"], json!([]));
        let health = app
            .call("GET", "/api/v1/health", &[["cookie", &session]], None)
            .await
            .json();
        assert_eq!(health["alerts"], 0);
    });
}

#[test]
fn set_up_and_check_refuse_a_phone_that_is_not_online() {
    test_app::run(async {
        let app = TestApp::start_with(b30_settings());
        let session = app.alice().await;
        for action in ["provision", "check"] {
            let response = app
                .call(
                    "POST",
                    &format!("/api/v1/phones/NOPE/{action}"),
                    &[["cookie", &session]],
                    Some("{}"),
                )
                .await;
            assert_eq!(response.status, StatusCode::CONFLICT, "{action}");
            assert_eq!(
                response.json(),
                json!({ "error": "phone NOPE is not online" })
            );
        }
    });
}

#[test]
fn set_up_and_check_refuse_requests_another_site_could_make() {
    test_app::run(async {
        let app = TestApp::start_with(b30_settings());
        let session = app.alice().await;
        let cookie = ["cookie", session.as_str()];
        let path = "/api/v1/phones/SPARELINE00001/provision";
        let refused: [Vec<[&str; 2]>; 5] = [
            vec![cookie],
            vec![
                cookie,
                ["content-type", "application/x-www-form-urlencoded"],
            ],
            vec![cookie, ["content-type", "text/plain"]],
            vec![
                cookie,
                ["content-type", "application/json"],
                ["sec-fetch-site", "cross-site"],
            ],
            vec![
                cookie,
                ["x-spareline", "1"],
                ["sec-fetch-site", "cross-site"],
            ],
        ];
        for headers in refused {
            let response = app.call("POST", path, &headers, None).await;
            assert_eq!(response.status, StatusCode::FORBIDDEN, "{headers:?}");
            assert_eq!(
                response.json(),
                json!({ "error": "send Content-Type: application/json or x-spareline: 1, from this site" })
            );
        }
        let check = app
            .call(
                "POST",
                "/api/v1/phones/SPARELINE00001/check",
                &[cookie],
                None,
            )
            .await;
        assert_eq!(check.status, StatusCode::FORBIDDEN);
        let marked = app
            .call(
                "POST",
                path,
                &[
                    cookie,
                    ["x-spareline", "1"],
                    ["sec-fetch-site", "same-origin"],
                ],
                None,
            )
            .await;
        assert_eq!(
            marked.status,
            StatusCode::CONFLICT,
            "the header passes the same-site rule; the phone isn't online"
        );
    });
}

#[test]
fn without_a_policy_the_status_has_no_profile_and_set_up_answers_409() {
    test_app::run(async {
        let app = TestApp::start();
        let session = app.alice().await;
        let status = app
            .call("GET", "/api/v1/status", &[["cookie", &session]], None)
            .await
            .json();
        assert_eq!(status["profile"], Value::Null);
        assert_eq!(status["dataThresholdBytes"], Value::Null);
        for action in ["provision", "check"] {
            let response = app
                .call(
                    "POST",
                    &format!("/api/v1/phones/SPARELINE00001/{action}"),
                    &[["cookie", &session]],
                    Some("{}"),
                )
                .await;
            assert_eq!(response.status, StatusCode::CONFLICT, "{action}");
            assert_eq!(
                response.json(),
                json!({ "error": "no policy profile is loaded (set POLICY_PROFILE to use phone rules)" })
            );
        }
    });
}

#[test]
fn a_check_on_an_online_phone_answers_its_report() {
    test_app::run(async {
        let app = TestApp::with_b30_and(b30_settings(), Monitoring::Running).await;
        let session = app.alice().await;
        let response = app
            .call(
                "POST",
                "/api/v1/phones/SPARELINE00001/check",
                &[["cookie", &session]],
                Some("{}"),
            )
            .await;
        assert_eq!(response.status, StatusCode::OK, "{}", response.text());
        let report = response.json();
        assert_eq!(report["reason"], "manual");
        assert!(
            report["error"].is_string(),
            "the stand-in doesn't answer the policy read: {report}"
        );
        let status = app
            .call("GET", "/api/v1/status", &[["cookie", &session]], None)
            .await;
        let text = status.text();
        assert_eq!(
            test_app::ordered_keys(&text, &["phones", "0", "policy"]),
            ["busy", "check", "provision", "dataAlarm"],
            "the policy view leaves out the serial, as the old schema did"
        );
    });
}
