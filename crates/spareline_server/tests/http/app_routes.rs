//! The inventory routes, the event stream, the docs and the web UI, ported from spareline-old's
//! `src/api/app.test.ts`. The phones, SIMs and numbers are rows a test writes, since no phone
//! is attached; live state is the phone manager's tests' business.

use std::time::Duration;
use std::{fs, str};

use axum::body::BodyDataStream;
use axum::http::StatusCode;
use axum::response::Response;
use serde_json::{Value, json};
use tokio::time;

use crate::test_app::{self, TestApp};

const SERIAL: &str = "SPARELINE00001";

#[test]
fn healthz_says_only_whether_adb_is_reachable_without_a_session() {
    test_app::run(async {
        let app = TestApp::start();
        let response = app.call("GET", "/healthz", &[], None).await;
        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(response.json(), json!({ "ok": false }));
        assert_eq!(
            response.header("content-type").as_deref(),
            Some("application/json")
        );
        let trailing = app.call("GET", "/healthz/", &[], None).await;
        assert_eq!(
            trailing.status,
            StatusCode::OK,
            "a trailing slash is ignored"
        );
        let head = app.call("HEAD", "/healthz", &[], None).await;
        assert_eq!(head.status, StatusCode::OK, "HEAD works on GET routes");
        let post = app.call("POST", "/healthz", &[], None).await;
        assert_eq!(post.status, StatusCode::NOT_FOUND);
        assert_eq!(post.json(), json!({ "error": "not found" }));
    });
}

#[test]
fn health_summarizes_adb_and_phones_for_a_signed_in_user() {
    test_app::run(async {
        let app = TestApp::start();
        app.add_phone();
        let session = app.alice().await;
        assert_eq!(
            app.call("GET", "/api/v1/health", &[], None).await.status,
            StatusCode::UNAUTHORIZED
        );
        let response = app
            .call("GET", "/api/v1/health", &[["cookie", &session]], None)
            .await;
        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(
            response.text(),
            r#"{"ok":false,"adbReachable":false,"phonesOnline":0,"phonesOffline":1,"alerts":0,"version":"test"}"#,
            "a phone this process hasn't seen online counts as offline"
        );
    });
}

#[test]
fn phones_show_the_inventory_in_the_old_schemas_order() {
    test_app::run(async {
        let app = TestApp::start();
        app.add_phone();
        let session = app.alice().await;
        let response = app
            .call("GET", "/api/v1/phones", &[["cookie", &session]], None)
            .await;
        let phones = response.json()["phones"].clone();
        let phone = &phones[0];
        assert_eq!(
            test_app::ordered_keys(&response.text(), &["phones", "0"]),
            [
                "id",
                "serial",
                "site",
                "manufacturer",
                "model",
                "androidVersion",
                "imeis",
                "location",
                "hubPort",
                "costCents",
                "acquiredOn",
                "status",
                "adbState",
                "statusSince",
                "firstSeenAt",
                "lastOnlineAt",
                "battery",
                "slots",
                "healthAt",
                "identityAt",
                "warnings",
                "probeError"
            ]
        );
        assert_eq!(phone["serial"], SERIAL);
        assert_eq!(
            phone["imeis"],
            json!(["350000000000006", "350000000000014"])
        );
        assert_eq!(
            phone["status"], "online",
            "the stored state, before this run sees it"
        );
        assert_eq!(phone["adbState"], "device");
        assert_eq!(phone["statusSince"], Value::Null);
        assert_eq!(phone["battery"], Value::Null);
        assert_eq!(
            phone["slots"],
            json!([{
                "slot": 0,
                "simState": null,
                "radio": null,
                "sim": { "id": 1, "iccid": "8901555000000000001", "carrier": "T-Mobile", "subId": 1 },
                "number": { "id": 1, "e164": "+15125550101", "source": "detected", "note": "" }
            }])
        );
        let one = app
            .call(
                "GET",
                &format!("/api/v1/phones/{SERIAL}"),
                &[["cookie", &session]],
                None,
            )
            .await;
        assert_eq!(one.json(), *phone);
        let missing = app
            .call("GET", "/api/v1/phones/NOPE", &[["cookie", &session]], None)
            .await;
        assert_eq!(missing.status, StatusCode::NOT_FOUND);
        assert_eq!(missing.json(), json!({ "error": "no phone NOPE" }));
    });
}

#[test]
fn editing_a_phone_records_who_signed_in() {
    test_app::run(async {
        let app = TestApp::start();
        app.add_phone();
        let session = app.alice().await;
        let path = format!("/api/v1/phones/{SERIAL}");
        let response = app
            .call(
                "PATCH",
                &path,
                &[["cookie", &session]],
                Some(r#"{"location":"A3"}"#),
            )
            .await;
        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(response.json()["location"], "A3");
        let edit: [String; 3] = app
            .sql
            .query_row(
                "SELECT login, entity, changes_json FROM inventory_edits",
                [],
                |row| Ok([row.get(0)?, row.get(1)?, row.get(2)?]),
            )
            .expect("one edit");
        assert_eq!(edit, ["alice", "phone", r#"{"location":"A3"}"#]);
        for (body, message) in [
            (
                r#"{"nickname":"x"}"#,
                "nickname: unknown field `nickname`, expected one of `location`, `hubPort`, \
                 `costCents`, `acquiredOn` at line 1 column 11",
            ),
            (
                r#"{"costCents":-5}"#,
                "costCents: expected a whole number of cents from 0 to 9007199254740991, got -5 \
                 at line 1 column 16",
            ),
            (
                r#"{"acquiredOn":"2026-13-01"}"#,
                "acquiredOn: expected a date written YYYY-MM-DD, got \"2026-13-01\" at line 1 \
                 column 27",
            ),
        ] {
            let refused = app
                .call("PATCH", &path, &[["cookie", &session]], Some(body))
                .await;
            assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
            assert_eq!(refused.json(), json!({ "error": message }));
        }
        let cleared = app
            .call(
                "PATCH",
                &path,
                &[["cookie", &session]],
                Some(r#"{"location":null,"costCents":1999,"acquiredOn":"2026-01-31"}"#),
            )
            .await;
        assert_eq!(cleared.json()["location"], Value::Null);
        assert_eq!(cleared.json()["costCents"], 1999);
        let missing = app
            .call(
                "PATCH",
                "/api/v1/phones/NOPE",
                &[["cookie", &session]],
                Some(r#"{"location":"A3"}"#),
            )
            .await;
        assert_eq!(missing.status, StatusCode::NOT_FOUND);
    });
}

/// A number's fields in the live build's order, which puts `phoneStatus` and `radio` last.
const NUMBER_FIELDS: [&str; 13] = [
    "id",
    "e164",
    "source",
    "note",
    "detectedE164",
    "confirmedBy",
    "createdAt",
    "sim",
    "phone",
    "messageCount",
    "lastMessage",
    "phoneStatus",
    "radio",
];

#[test]
fn numbers_detected_overridden_by_hand_conflicting_and_cleared() {
    test_app::run(async {
        let app = TestApp::start();
        app.add_phone();
        let session = app.alice().await;
        let cookie = [["cookie", session.as_str()]];
        let list = app.call("GET", "/api/v1/numbers", &cookie, None).await;
        let number = list.json()["numbers"][0].clone();
        assert_eq!(
            test_app::ordered_keys(&list.text(), &["numbers", "0"]),
            NUMBER_FIELDS,
            "the live build puts phoneStatus and radio last"
        );
        assert_eq!(
            number["phone"],
            json!({ "serial": SERIAL, "model": "S6702X", "slot": 0, "removedAt": null })
        );
        assert_eq!(number["phoneStatus"], "offline", "not seen online this run");
        let one = app
            .call("GET", "/api/v1/numbers/+15125550101", &cookie, None)
            .await;
        assert_eq!(one.status, StatusCode::OK);
        assert_eq!(test_app::ordered_keys(&one.text(), &[]), NUMBER_FIELDS);
        let set = app
            .call(
                "PATCH",
                "/api/v1/numbers/1",
                &cookie,
                Some(r#"{"e164":"(512) 555-0199","note":"Craigslist"}"#),
            )
            .await;
        assert_eq!(set.status, StatusCode::OK);
        assert_eq!(set.json()["e164"], "+15125550199");
        assert_eq!(set.json()["source"], "manual");
        assert_eq!(set.json()["detectedE164"], "+15125550101");
        assert_eq!(set.json()["note"], "Craigslist");
        assert_eq!(set.json()["confirmedBy"], "alice");
        for raw in ["call me", "+0123456789"] {
            let body = json!({ "e164": raw }).to_string();
            let refused = app
                .call("PATCH", "/api/v1/numbers/1", &cookie, Some(&body))
                .await;
            assert_eq!(refused.status, StatusCode::BAD_REQUEST, "{raw}");
            assert_eq!(
                refused.json(),
                json!({ "error": format!("\"{raw}\" is not a phone number in E.164 or 10-digit US form") })
            );
        }
        app.sql
            .execute(
                "INSERT INTO numbers (id, e164, e164_source, created_at) VALUES (2, '+15125550177', 'manual', '2026-10-22T00:00:00.000Z')",
                [],
            )
            .expect("another number");
        let conflict = app
            .call(
                "PATCH",
                "/api/v1/numbers/1",
                &cookie,
                Some(r#"{"e164":"+15125550177"}"#),
            )
            .await;
        assert_eq!(conflict.status, StatusCode::CONFLICT);
        assert_eq!(
            conflict.json(),
            json!({ "error": "+15125550177 already belongs to number 2" })
        );
        let cleared = app
            .call(
                "PATCH",
                "/api/v1/numbers/1",
                &cookie,
                Some(r#"{"e164":null}"#),
            )
            .await;
        assert_eq!(cleared.json()["e164"], "+15125550101");
        let missing = app.call("GET", "/api/v1/numbers/99", &cookie, None).await;
        assert_eq!(missing.json(), json!({ "error": "no number 99" }));
        let bare = app
            .call("GET", "/api/v1/numbers/5125550101", &cookie, None)
            .await;
        assert_eq!(
            bare.status,
            StatusCode::NOT_FOUND,
            "bare digits aren't a key here"
        );
    });
}

#[test]
fn a_numbers_note_is_edited_alone_and_shows_wherever_the_number_does() {
    test_app::run(async {
        let app = TestApp::start();
        app.add_phone();
        let session = app.alice().await;
        let cookie = [["cookie", session.as_str()]];
        let response = app
            .call(
                "PATCH",
                "/api/v1/numbers/+15125550101",
                &cookie,
                Some(r#"{"note":"Facebook ad, blue couch"}"#),
            )
            .await;
        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(response.json()["source"], "detected");
        let one = app.call("GET", "/api/v1/numbers/1", &cookie, None).await;
        assert_eq!(one.json()["note"], "Facebook ad, blue couch");
        let phones = app.call("GET", "/api/v1/phones", &cookie, None).await;
        assert_eq!(
            phones.json()["phones"][0]["slots"][0]["number"]["note"],
            "Facebook ad, blue couch"
        );
        let edit: [String; 3] = app
            .sql
            .query_row(
                "SELECT login, entity, changes_json FROM inventory_edits",
                [],
                |row| Ok([row.get(0)?, row.get(1)?, row.get(2)?]),
            )
            .expect("one edit");
        assert_eq!(
            edit,
            ["alice", "number", r#"{"note":"Facebook ad, blue couch"}"#]
        );
        let cleared = app
            .call(
                "PATCH",
                "/api/v1/numbers/1",
                &cookie,
                Some(r#"{"note":""}"#),
            )
            .await;
        assert_eq!(cleared.json()["note"], "");
        let long = json!({ "note": "x".repeat(2001) }).to_string();
        let refused = app
            .call("PATCH", "/api/v1/numbers/1", &cookie, Some(&long))
            .await;
        assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    });
}

#[test]
fn sims_by_id_or_iccid() {
    test_app::run(async {
        let app = TestApp::start();
        app.add_phone();
        let session = app.alice().await;
        let cookie = [["cookie", session.as_str()]];
        let sim = app
            .call("GET", "/api/v1/sims/8901555000000000001", &cookie, None)
            .await
            .json();
        assert_eq!(sim["id"], 1);
        assert_eq!(sim["placement"]["serial"], SERIAL);
        assert_eq!(sim["placement"]["slot"], 0);
        assert_eq!(sim["placement"]["subId"], 1);
        let spare = app
            .call(
                "PATCH",
                "/api/v1/sims/1",
                &cookie,
                Some(r#"{"status":"spare"}"#),
            )
            .await;
        assert_eq!(spare.json()["status"], "spare");
        for body in [r#"{"status":"lost"}"#, r#"{"note":"prepaid"}"#] {
            let refused = app
                .call("PATCH", "/api/v1/sims/1", &cookie, Some(body))
                .await;
            assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        }
        let missing = app.call("GET", "/api/v1/sims/77", &cookie, None).await;
        assert_eq!(missing.json(), json!({ "error": "no SIM 77" }));
        let list = app.call("GET", "/api/v1/sims", &cookie, None).await;
        assert_eq!(list.json()["sims"][0]["iccid"], "8901555000000000001");
    });
}

#[test]
fn status_shows_live_adb_state() {
    test_app::run(async {
        let app = TestApp::start();
        let session = app.alice().await;
        let response = app
            .call("GET", "/api/v1/status", &[["cookie", &session]], None)
            .await;
        let status = response.json();
        assert_eq!(
            test_app::ordered_keys(&response.text(), &[]),
            ["adb", "phones", "alerts", "dataThresholdBytes", "profile"]
        );
        assert_eq!(status["adb"]["reachable"], false);
        assert_eq!(status["adb"]["error"], "not connected yet");
        assert_eq!(status["phones"], json!([]));
        assert_eq!(status["alerts"], json!([]));
        assert_eq!(status["dataThresholdBytes"], Value::Null);
        assert_eq!(status["profile"], Value::Null);
    });
}

/// Reads `response`'s stream until it holds `needle`, or panics when it ends first.
async fn read_until(stream: &mut BodyDataStream, text: &mut String, needle: &str) {
    use futures_util::StreamExt as _;

    while !text.contains(needle) {
        let chunk = stream
            .next()
            .await
            .unwrap_or_else(|| panic!("the stream ended before {needle:?}: {text}"))
            .expect("the stream reads");
        text.push_str(str::from_utf8(&chunk).expect("UTF-8"));
    }
}

/// The ids of the events in `text`, not the cursor of the opening block.
fn ids(text: &str) -> Vec<String> {
    text.split("\n\n")
        .filter(|block| block.contains("\nevent: "))
        .filter_map(|block| block.strip_prefix("id: "))
        .map(|rest| rest.lines().next().unwrap_or("").to_owned())
        .collect()
}

async fn open(app: &TestApp, path: &str, headers: &[[&str; 2]]) -> BodyDataStream {
    let response: Response = app.send("GET", path, headers, None).await;
    assert_eq!(response.status(), StatusCode::OK, "{path} opens");
    assert_eq!(
        response.headers()["content-type"],
        "text/event-stream",
        "an event stream has no charset"
    );
    assert_eq!(
        response.headers()["cache-control"],
        "no-cache",
        "nothing caches it"
    );
    assert_eq!(
        response.headers()["x-accel-buffering"],
        "no",
        "a proxy doesn't buffer it"
    );
    response.into_body().into_data_stream()
}

#[test]
fn the_events_stream_replays_stored_events_then_new_ones_with_a_heartbeat() {
    test_app::run(async {
        let app = TestApp::start();
        let session = app.alice().await;
        let cookie = [["cookie", session.as_str()]];
        let stored = app.store_events(3, "phone.added").await;
        let mut stream = open(&app, "/api/v1/events?after=0", &cookie).await;
        let mut text = String::new();
        read_until(&mut stream, &mut text, "retry: 3000\n").await;
        assert!(text.starts_with("id: 1\nevent: device\ndata: {\"id\":1,\"phoneId\":null,\"serial\":null,\"at\":\"2026-10-22T00:00:00.000Z\",\"kind\":\"phone.added\",\"detail\":{\"i\":0}}\n\n"), "{text}");
        read_until(&mut stream, &mut text, "retry: 3000\nid: 3\n\n").await;
        let fresh = app.store_events(1, "adb.unreachable").await;
        let recorded = app
            .state
            .db()
            .call(|db| db.device_events().after(3, 10))
            .await
            .expect("the event reads");
        app.state.buses().publish_device_events(&recorded);
        read_until(&mut stream, &mut text, "adb.unreachable").await;
        assert_eq!(ids(&text), ["1", "2", "3", "4"]);
        assert_eq!(fresh, [stored[2] + 1]);
        time::advance(Duration::from_secs(15)).await;
        read_until(&mut stream, &mut text, ": keep-alive\n\n").await;
    });
}

#[test]
fn after_replays_every_later_event_in_order_not_just_the_newest_page() {
    test_app::run(async {
        let app = TestApp::start();
        let session = app.alice().await;
        let stored = app.store_events(1150, "adb.reachable").await;
        let last = *stored.last().expect("events");
        let mut stream = open(&app, "/api/v1/events?after=10", &[["cookie", &session]]).await;
        let mut text = String::new();
        read_until(
            &mut stream,
            &mut text,
            &format!("retry: 3000\nid: {last}\n\n"),
        )
        .await;
        let expected: Vec<String> = (11..=last).map(|id| id.to_string()).collect();
        assert_eq!(ids(&text), expected);
    });
}

#[test]
fn latest_replays_the_newest_events_and_last_event_id_resumes_after_its_id() {
    test_app::run(async {
        let app = TestApp::start();
        let session = app.alice().await;
        let last = *app
            .store_events(30, "adb.reachable")
            .await
            .last()
            .expect("events");
        let mut latest = open(&app, "/api/v1/events?latest=3", &[["cookie", &session]]).await;
        let mut text = String::new();
        read_until(&mut latest, &mut text, "retry: 3000\n").await;
        assert_eq!(
            ids(&text),
            [last - 2, last - 1, last].map(|id| id.to_string())
        );
        let resume = (last - 5).to_string();
        let mut resumed = open(
            &app,
            "/api/v1/events?latest=3&after=1",
            &[["cookie", &session], ["last-event-id", &resume]],
        )
        .await;
        let mut text = String::new();
        read_until(&mut resumed, &mut text, "retry: 3000\n").await;
        assert_eq!(
            ids(&text),
            (last - 4..=last)
                .map(|id| id.to_string())
                .collect::<Vec<_>>(),
            "Last-Event-ID wins over after"
        );
        let mut live = open(&app, "/api/v1/events", &[["cookie", &session]]).await;
        let mut text = String::new();
        read_until(
            &mut live,
            &mut text,
            &format!("retry: 3000\nid: {last}\n\n"),
        )
        .await;
        app.store_events(1, "adb.unreachable").await;
        let recorded = app
            .state
            .db()
            .call(move |db| db.device_events().after(last, 10))
            .await
            .expect("the event reads");
        app.state.buses().publish_device_events(&recorded);
        read_until(&mut live, &mut text, "adb.unreachable").await;
        assert_eq!(ids(&text), [(last + 1).to_string()]);
        let bad = app
            .call(
                "GET",
                "/api/v1/events?latest=501",
                &[["cookie", &session]],
                None,
            )
            .await;
        assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            bad.json(),
            json!({ "error": "latest: expected an integer from 0 to 500, got \"501\"" })
        );
    });
}

#[test]
fn a_stream_that_misses_bus_events_reads_them_from_the_database() {
    test_app::run(async {
        let app = TestApp::start();
        let session = app.alice().await;
        let mut stream = open(&app, "/api/v1/events", &[["cookie", &session]]).await;
        let mut text = String::new();
        read_until(&mut stream, &mut text, "retry: 3000\nid: 0\n\n").await;
        app.store_events(3, "adb.reachable").await;
        let third = app
            .state
            .db()
            .call(|db| db.device_events().after(2, 10))
            .await
            .expect("the event reads");
        app.state.buses().publish_device_events(&third);
        read_until(&mut stream, &mut text, "id: 3\n").await;
        assert_eq!(
            ids(&text),
            ["1", "2", "3"],
            "events 1 and 2 were never published"
        );
    });
}

#[test]
fn an_asset_path_that_is_a_directory_is_a_404_not_a_server_error() {
    test_app::run(async {
        let app = TestApp::start();
        fs::create_dir(app.web_dir.join("assets/nested")).expect("a directory");
        for path in ["/assets/", "/assets/nested"] {
            let response = app.call("GET", path, &[], None).await;
            assert_eq!(response.status, StatusCode::NOT_FOUND, "{path}");
            assert_eq!(response.text(), "not found");
        }
    });
}

#[test]
fn the_web_ui_is_served_for_client_routes_assets_are_immutable_and_paths_stay_inside() {
    test_app::run(async {
        let app = TestApp::start();
        let session = app.alice().await;
        let page = app.call("GET", "/numbers/1", &[], None).await;
        assert_eq!(page.status, StatusCode::OK);
        assert!(page.text().contains("id=root"));
        assert_eq!(page.header("cache-control").as_deref(), Some("no-cache"));
        let root = app.call("GET", "/", &[], None).await;
        assert!(root.text().contains("id=root"));
        let asset = app.call("GET", "/assets/main-abc123.js", &[], None).await;
        assert_eq!(
            asset.header("cache-control").as_deref(),
            Some("public, max-age=31536000, immutable")
        );
        assert_eq!(asset.text(), "console.log(1)");
        let escape = app
            .call("GET", "/assets/..%2F..%2Fdata%2Fspareline.db", &[], None)
            .await;
        assert_eq!(escape.status, StatusCode::NOT_FOUND);
        let unknown = app
            .call("GET", "/api/v1/nothing", &[["cookie", &session]], None)
            .await;
        assert_eq!(unknown.status, StatusCode::NOT_FOUND);
        assert_eq!(unknown.json(), json!({ "error": "not found" }));
        let docs = app.call("GET", "/api/docs", &[], None).await;
        assert_eq!(docs.status, StatusCode::OK);
        assert!(docs.text().contains("api-reference"));
        let document = app.call("GET", "/api/docs/json", &[], None).await.json();
        assert_eq!(document["info"]["version"], "test");
        fs::remove_file(app.web_dir.join("index.html")).expect("the page is removed");
        let unbuilt = app.call("GET", "/numbers", &[], None).await;
        assert_eq!(unbuilt.status, StatusCode::SERVICE_UNAVAILABLE);
    });
}

#[test]
fn an_edit_made_with_an_api_key_records_the_user_and_the_key() {
    test_app::run(async {
        let app = TestApp::start();
        app.add_phone();
        let session = app.alice().await;
        let key = app.create_key(&session, "inventory script", "full").await;
        let bearer = format!("Bearer {key}");
        let response = app
            .call(
                "PATCH",
                &format!("/api/v1/phones/{SERIAL}"),
                &[["authorization", &bearer]],
                Some(r#"{"location":"B1"}"#),
            )
            .await;
        assert_eq!(response.status, StatusCode::OK);
        let login: String = app
            .sql
            .query_row(
                "SELECT login FROM inventory_edits ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .expect("one edit");
        assert_eq!(login, "alice (key inventory script)");
    });
}

#[test]
fn health_counts_the_b30_online_once_the_phone_manager_sees_it() {
    test_app::run(async {
        let app = TestApp::with_b30().await;
        let session = app.alice().await;
        let response = app
            .call("GET", "/api/v1/health", &[["cookie", &session]], None)
            .await;
        assert_eq!(
            response.text(),
            r#"{"ok":true,"adbReachable":true,"phonesOnline":1,"phonesOffline":0,"alerts":0,"version":"test"}"#
        );
        let healthz = app.call("GET", "/healthz", &[], None).await;
        assert_eq!(healthz.json(), json!({ "ok": true }));
    });
}

#[test]
fn phones_show_the_inventory_joined_with_live_state() {
    test_app::run(async {
        let app = TestApp::with_b30().await;
        let session = app.alice().await;
        let response = app
            .call("GET", "/api/v1/phones", &[["cookie", &session]], None)
            .await;
        let phone = response.json()["phones"][0].clone();
        assert_eq!(phone["serial"], SERIAL);
        assert_eq!(phone["model"], "S6702X");
        assert_eq!(phone["androidVersion"], "14");
        assert_eq!(
            phone["imeis"],
            json!(["350000000000006", "350000000000014"])
        );
        assert_eq!(phone["status"], "online");
        assert_eq!(phone["adbState"], "device");
        assert!(phone["statusSince"].is_string(), "{phone}");
        assert_eq!(
            phone["battery"],
            json!({ "level": 85, "temperatureC": 22.1, "status": "not_charging", "plugged": ["usb"] })
        );
        let slots = phone["slots"].as_array().expect("slots").clone();
        assert_eq!(slots.len(), 2, "{phone}");
        assert_eq!(slots[0]["simState"], "LOADED");
        assert_eq!(slots[0]["radio"]["voice"], "out_of_service");
        assert_eq!(slots[0]["radio"]["signalLevel"], 1);
        assert_eq!(slots[0]["radio"]["signalDbm"], -122);
        assert_eq!(
            slots[0]["sim"],
            json!({ "id": 1, "iccid": "8901555000000000001", "carrier": "T-Mobile", "subId": 1 })
        );
        assert_eq!(
            slots[0]["number"],
            json!({ "id": 1, "e164": "+15125550101", "source": "detected", "note": "" })
        );
        assert_eq!(slots[1]["simState"], "ABSENT");
        assert_eq!(slots[1]["radio"]["voice"], "power_off");
        assert_eq!(slots[1]["sim"], Value::Null);
        assert_eq!(slots[1]["number"], Value::Null);
        let numbers = app
            .call("GET", "/api/v1/numbers", &[["cookie", &session]], None)
            .await
            .json();
        let number = &numbers["numbers"][0];
        assert_eq!(number["phoneStatus"], "online");
        assert_eq!(number["radio"]["slot"], 0);
        assert_eq!(number["radio"]["voice"], "out_of_service");
    });
}

#[test]
fn status_shows_live_adb_and_phone_state() {
    test_app::run(async {
        let app = TestApp::with_b30().await;
        let session = app.alice().await;
        let response = app
            .call("GET", "/api/v1/status", &[["cookie", &session]], None)
            .await;
        let status = response.json();
        assert_eq!(status["adb"]["reachable"], true);
        assert_eq!(status["adb"]["version"], 41);
        assert_eq!(status["adb"]["error"], Value::Null);
        let phone = &status["phones"][0];
        assert_eq!(
            test_app::ordered_keys(&response.text(), &["phones", "0"]),
            [
                "serial",
                "status",
                "adbState",
                "since",
                "usb",
                "battery",
                "radios",
                "simStates",
                "healthAt",
                "identityAt",
                "probeError",
                "policy"
            ]
        );
        assert_eq!(phone["serial"], SERIAL);
        assert_eq!(phone["status"], "online");
        assert_eq!(phone["adbState"], "device");
        assert_eq!(phone["usb"], "1-1");
        assert_eq!(phone["simStates"], json!(["LOADED", "ABSENT"]));
        assert_eq!(
            phone["policy"],
            Value::Null,
            "the monitor hasn't seen the phone"
        );
    });
}
