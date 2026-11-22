//! Texts: pages, long polls, search, attachments and the text half of the event stream, ported
//! from spareline-old's `src/api/messages.test.ts`. The texts are made-up ones stored the way
//! ingest stores them, then published as ingest publishes them.

use std::time::Duration;

use axum::body::BodyDataStream;
use axum::http::StatusCode;
use futures_util::future;
use serde_json::{Value, json};
use spareline_core::Timestamp;
use spareline_db::{AttachmentStore, Message, NewAttachment, NewMessage, Provider};
use tokio::time::{self, Instant};

use crate::test_app::{self, TestApp};

const NUMBER: &str = "+15125550101";

/// The made-up text `n`: provider id `n + 3`, from `+1512555015<n % 10>`, received `n` minutes
/// after 2026-10-22T09:00Z, on sub 1.
fn new_text(n: i64, body: &str) -> NewMessage {
    let base = Timestamp::parse_iso("2026-10-22T09:00:00.000Z").expect("a time");
    NewMessage {
        provider: Provider::Sms,
        provider_id: n + 3,
        sub_id: Some(1),
        sender: Some(format!("+1512555015{}", n % 10)),
        body: body.to_owned(),
        subject: None,
        sent_at: Some(base.plus(Duration::from_mins(u64::try_from(n).expect("n >= 0")))),
        received_at: base.plus(Duration::from_secs(
            60 * u64::try_from(n).expect("n >= 0") + 14,
        )),
        attachments: Vec::new(),
    }
}

/// Stores `texts` from phone `phone_id` and publishes the new ones, as ingest does.
async fn ingest(app: &TestApp, phone_id: i64, texts: Vec<NewMessage>) -> Vec<Message> {
    let now = app.state.clock().now();
    let stored = app
        .state
        .db()
        .call(move |db| db.messages().insert(phone_id, &texts, now))
        .await
        .expect("the texts are stored");
    app.state.buses().publish_texts(&stored);
    stored
}

/// The phone with its number and texts 1 to 8, `Fake text <n>`.
async fn eight_texts(app: &TestApp) -> String {
    app.add_phone();
    let texts: Vec<NewMessage> = (1..=8)
        .map(|n| new_text(n, &format!("Fake text {n}")))
        .collect();
    ingest(app, 1, texts).await;
    app.alice().await
}

fn seqs(page: &Value) -> Vec<i64> {
    page["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .map(|message| message["seq"].as_i64().expect("a seq"))
        .collect()
}

#[test]
fn numbers_list_message_counts_and_the_last_message() {
    test_app::run(async {
        let app = TestApp::start();
        let session = eight_texts(&app).await;
        let numbers = app
            .call("GET", "/api/v1/numbers", &[["cookie", &session]], None)
            .await
            .json();
        let number = &numbers["numbers"][0];
        assert_eq!(number["e164"], NUMBER);
        assert_eq!(number["messageCount"], 8);
        assert_eq!(number["lastMessage"]["seq"], 8);
        assert_eq!(number["lastMessage"]["from"], "+15125550158");
        assert_eq!(number["lastMessage"]["preview"], "Fake text 8");
    });
}

#[test]
fn a_numbers_messages_newest_first_paging_with_before_and_any_key_form() {
    test_app::run(async {
        let app = TestApp::start();
        let session = eight_texts(&app).await;
        let cookie = [["cookie", session.as_str()]];
        let response = app
            .call(
                "GET",
                "/api/v1/numbers/%2B15125550101/messages?limit=3",
                &cookie,
                None,
            )
            .await;
        let page = response.json();
        assert_eq!(seqs(&page), [8, 7, 6]);
        assert_eq!(page["cursor"], 8);
        let first = &page["messages"][0];
        assert_eq!(
            test_app::ordered_keys(&response.text(), &["messages", "0"]),
            [
                "seq",
                "provider",
                "providerId",
                "phone",
                "subId",
                "simId",
                "number",
                "from",
                "body",
                "subject",
                "sentAt",
                "receivedAt",
                "ingestedAt",
                "attachments"
            ]
        );
        assert_eq!(first["provider"], "sms");
        assert_eq!(first["providerId"], 11);
        assert_eq!(first["phone"], "SPARELINE00001");
        assert_eq!(
            first["number"],
            json!({ "id": 1, "e164": NUMBER, "note": "" })
        );
        assert_eq!(first["sentAt"], "2026-10-22T09:08:00.000Z");
        assert_eq!(first["receivedAt"], "2026-10-22T09:08:14.000Z");
        assert_eq!(first["attachments"], json!([]));
        let older = app
            .call(
                "GET",
                &format!("/api/v1/numbers/{NUMBER}/messages?before=6"),
                &cookie,
                None,
            )
            .await;
        assert_eq!(seqs(&older.json()), [5, 4, 3, 2, 1]);
        for key in ["15125550101", "5125550101", "1", "%2B1%20512%20555%200101"] {
            let page = app
                .call(
                    "GET",
                    &format!("/api/v1/numbers/{key}/messages?limit=1"),
                    &cookie,
                    None,
                )
                .await;
            assert_eq!(seqs(&page.json()), [8], "{key}");
        }
        for key in ["+15125550188", "99", "abc"] {
            let response = app
                .call(
                    "GET",
                    &format!("/api/v1/numbers/{key}/messages"),
                    &cookie,
                    None,
                )
                .await;
            assert_eq!(response.status, StatusCode::NOT_FOUND, "{key}");
            assert_eq!(
                response.json(),
                json!({ "error": format!("no number {key}") })
            );
        }
        let wait = app
            .call(
                "GET",
                &format!("/api/v1/numbers/{NUMBER}/messages?wait=31"),
                &cookie,
                None,
            )
            .await;
        assert_eq!(wait.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            wait.json(),
            json!({ "error": "wait: expected an integer from 0 to 30, got \"31\"" })
        );
    });
}

#[test]
fn a_long_poll_returns_as_soon_as_a_text_arrives_oldest_first() {
    test_app::run(async {
        let app = TestApp::start();
        let session = eight_texts(&app).await;
        let cookie = [["cookie", session.as_str()]];
        let after = app
            .call(
                "GET",
                &format!("/api/v1/numbers/{NUMBER}/messages?after=6"),
                &cookie,
                None,
            )
            .await;
        assert_eq!(seqs(&after.json()), [7, 8]);
        let path = format!("/api/v1/numbers/{NUMBER}/messages?after=8&wait=10");
        let waiting = app.call("GET", &path, &cookie, None);
        let arriving = async {
            time::sleep(Duration::from_millis(100)).await;
            ingest(&app, 1, vec![new_text(9, "Still available?")]).await;
        };
        let started = Instant::now();
        let (page, ()) = future::join(waiting, arriving).await;
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "it answered at once"
        );
        let page = page.json();
        assert_eq!(seqs(&page), [9]);
        assert_eq!(page["cursor"], 9);
        assert_eq!(page["messages"][0]["body"], "Still available?");
    });
}

#[test]
fn a_long_poll_times_out_cleanly_and_ignores_other_numbers_texts() {
    test_app::run(async {
        let app = TestApp::start();
        let session = eight_texts(&app).await;
        let cookie = [["cookie", session.as_str()]];
        app.sql
            .execute(
                "INSERT INTO phones (id, serial, site, last_state, first_seen_at) VALUES (2, 'OTHER', 'home', 'device', '2026-10-22T00:00:00.000Z')",
                [],
            )
            .expect("another phone");
        let path = format!("/api/v1/numbers/{NUMBER}/messages?after=8&wait=1");
        let waiting = app.call("GET", &path, &cookie, None);
        let arriving = async {
            time::sleep(Duration::from_millis(100)).await;
            ingest(&app, 2, vec![new_text(1, "Wrong number")]).await;
        };
        let started = Instant::now();
        let (page, ()) = future::join(waiting, arriving).await;
        assert_eq!(page.json(), json!({ "messages": [], "cursor": 8 }));
        let elapsed = started.elapsed();
        assert!(
            elapsed >= Duration::from_secs(1) && elapsed < Duration::from_secs(2),
            "{elapsed:?}"
        );
        let all = app
            .call("GET", "/api/v1/messages?after=8", &cookie, None)
            .await
            .json();
        assert_eq!(seqs(&all), [9]);
        assert_eq!(
            all["messages"][0]["number"],
            Value::Null,
            "an unassigned SIM's text"
        );
    });
}

#[test]
fn a_long_poll_ends_empty_when_its_session_ends() {
    test_app::run(async {
        let app = TestApp::start();
        let session = eight_texts(&app).await;
        let cookie = [["cookie", session.as_str()]];
        let path = format!("/api/v1/numbers/{NUMBER}/messages?after=8&wait=30");
        let waiting = app.call("GET", &path, &cookie, None);
        let signing_out = async {
            time::sleep(Duration::from_millis(100)).await;
            app.call("POST", "/api/v1/auth/logout", &cookie, Some("{}"))
                .await
        };
        let started = Instant::now();
        let (page, _) = future::join(waiting, signing_out).await;
        assert_eq!(page.json(), json!({ "messages": [], "cursor": 8 }));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "it ended on the sign-out"
        );
    });
}

#[test]
fn a_stopping_service_answers_waiting_long_polls_and_ends_open_streams_at_once() {
    use futures_util::StreamExt as _;

    test_app::run(async {
        let app = TestApp::start();
        let session = eight_texts(&app).await;
        let cookie = [["cookie", session.as_str()]];
        let mut stream = open(&app, "/api/v1/events?types=message", &cookie).await;
        let mut text = String::new();
        read_until(&mut stream, &mut text, "retry: 3000\n").await;
        let path = format!("/api/v1/numbers/{NUMBER}/messages?after=8&wait=30");
        let waiting = app.call("GET", &path, &cookie, None);
        let stopping = async {
            time::sleep(Duration::from_millis(100)).await;
            app.state.shut_down();
        };
        let started = Instant::now();
        let (page, ()) = future::join(waiting, stopping).await;
        assert_eq!(page.status, StatusCode::OK);
        assert_eq!(page.json(), json!({ "messages": [], "cursor": 8 }));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "it answered on the stop, not after its 30 s wait"
        );
        let mut rest = String::new();
        while let Some(chunk) = stream.next().await {
            rest.push_str(&String::from_utf8_lossy(&chunk.expect("the stream reads")));
        }
        assert_eq!(rest, "", "the stream ends without another keep-alive");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "it ended on the stop"
        );
        let late = app.call("GET", &path, &cookie, None).await;
        assert_eq!(
            late.json(),
            json!({ "messages": [], "cursor": 8 }),
            "a long poll that starts during the stop answers at once"
        );
        assert!(started.elapsed() < Duration::from_secs(1));
    });
}

#[test]
fn a_stream_that_fell_behind_catches_up_from_the_database_500_events_at_a_time() {
    use futures_util::StreamExt as _;

    test_app::run(async {
        let app = TestApp::start();
        let session = app.alice().await;
        let mut stream = open(&app, "/api/v1/events", &[["cookie", &session]]).await;
        let mut text = String::new();
        read_until(&mut stream, &mut text, "retry: 3000\n").await;
        let stored = app.store_events(1200, "adb.reachable").await;
        let last = *stored.last().expect("events");
        let newest = app
            .state
            .db()
            .call(move |db| db.device_events().after(last - 1, 1))
            .await
            .expect("the event reads");
        app.state.buses().publish_device_events(&newest);
        let mut sizes = Vec::new();
        let mut seen = Vec::new();
        while seen.last() != Some(&last.to_string()) {
            let chunk = stream
                .next()
                .await
                .expect("the stream goes on")
                .expect("the stream reads");
            let events = ids(&String::from_utf8_lossy(&chunk));
            sizes.push(events.len());
            seen.extend(events);
        }
        assert_eq!(sizes, [500, 500, 200], "one chunk per page of the catch-up");
        let expected: Vec<String> = stored.iter().map(ToString::to_string).collect();
        assert_eq!(seen, expected, "every event once, in order");
    });
}

/// What a replaying stream sent around the moment something ended it.
struct EndedReplay {
    /// How many events the first chunk held.
    first: usize,
    /// Everything the stream sent after that.
    rest: String,
}

/// Opens a stream that replays `backlog` device events from the start, reads its first chunk,
/// runs `end`, and reads the stream to its end.
async fn replay_ended_by<F: Future<Output = ()>>(
    app: &TestApp,
    session: &str,
    backlog: usize,
    end: F,
) -> EndedReplay {
    use futures_util::StreamExt as _;

    app.store_events(backlog, "adb.reachable").await;
    let mut stream = open(app, "/api/v1/events?after=0", &[["cookie", session]]).await;
    let first = stream
        .next()
        .await
        .expect("a first page")
        .expect("the stream reads");
    let first = ids(&String::from_utf8_lossy(&first)).len();
    end.await;
    let mut rest = String::new();
    while let Some(chunk) = stream.next().await {
        rest.push_str(&String::from_utf8_lossy(&chunk.expect("the stream reads")));
    }
    EndedReplay { first, rest }
}

#[test]
fn a_stop_ends_a_stream_in_the_middle_of_its_replay() {
    test_app::run(async {
        let app = TestApp::start();
        let session = app.alice().await;
        let EndedReplay { first, rest } =
            replay_ended_by(&app, &session, 1500, async { app.state.shut_down() }).await;
        assert_eq!(first, 500, "the first page");
        assert_eq!(
            rest, "",
            "no further page, and no opening line, after the stop"
        );
    });
}

#[test]
fn signing_out_ends_a_stream_in_the_middle_of_its_replay() {
    test_app::run(async {
        let app = TestApp::start();
        let session = app.alice().await;
        let cookie = [["cookie", session.as_str()]];
        let signing_out = async {
            let out = app
                .call("POST", "/api/v1/auth/logout", &cookie, Some("{}"))
                .await;
            assert_eq!(out.status, StatusCode::OK);
        };
        let EndedReplay { first, rest } = replay_ended_by(&app, &session, 2500, signing_out).await;
        assert_eq!(first, 500, "the first page");
        let more = ids(&rest).len();
        assert!(
            more <= 500,
            "at most the page already being read follows the sign-out, not {more} more events"
        );
        assert!(!rest.contains("retry:"), "the stream never opened: {rest}");
    });
}

#[test]
fn all_messages_newest_first_after_and_search_and_before_with_after_is_refused() {
    test_app::run(async {
        let app = TestApp::start();
        let session = eight_texts(&app).await;
        let cookie = [["cookie", session.as_str()]];
        let get = async |path: &str| app.call("GET", path, &cookie, None).await.json();
        assert_eq!(seqs(&get("/api/v1/messages?limit=2").await), [8, 7]);
        let after = get("/api/v1/messages?after=6&limit=1").await;
        assert_eq!(seqs(&after), [7]);
        assert_eq!(after["cursor"], 7);
        assert_eq!(seqs(&get("/api/v1/messages?q=fake%20TEXT%203").await), [3]);
        assert_eq!(seqs(&get("/api/v1/messages?q=5550152").await), [2]);
        app.call(
            "PATCH",
            "/api/v1/numbers/1",
            &cookie,
            Some(r#"{"note":"Facebook ad, blue couch"}"#),
        )
        .await;
        let by_note = get("/api/v1/messages?q=facebook%20AD").await;
        assert_eq!(seqs(&by_note).len(), 8);
        assert_eq!(
            by_note["messages"][0]["number"]["note"],
            "Facebook ad, blue couch"
        );
        assert_eq!(
            seqs(&get("/api/v1/messages?q=craigslist").await),
            Vec::<i64>::new()
        );
        assert_eq!(
            seqs(&get("/api/v1/messages?q=%20%20").await).len(),
            8,
            "a blank q"
        );
        assert_eq!(
            seqs(&get("/api/v1/messages?q=%28512%29%20555-0101").await).len(),
            8,
            "digits and phone punctuation match the number"
        );
        for path in [
            "/api/v1/messages?before=5&after=2".to_owned(),
            format!("/api/v1/numbers/{NUMBER}/messages?before=5&after=2"),
        ] {
            let response = app.call("GET", &path, &cookie, None).await;
            assert_eq!(response.status, StatusCode::BAD_REQUEST, "{path}");
            assert_eq!(
                response.json(),
                json!({ "error": "use before or after, not both" })
            );
        }
        let long = format!("/api/v1/messages?q={}", "x".repeat(201));
        let refused = app.call("GET", &long, &cookie, None).await;
        assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
        let limit = app
            .call("GET", "/api/v1/messages?limit=0", &cookie, None)
            .await;
        assert_eq!(
            limit.json(),
            json!({ "error": "limit: expected an integer from 1 to 500, got \"0\"" })
        );
        let unknown = app.call("GET", "/api/v1/messages/3", &cookie, None).await;
        assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    });
}

async fn open(app: &TestApp, path: &str, headers: &[[&str; 2]]) -> BodyDataStream {
    let response = app.send("GET", path, headers, None).await;
    assert_eq!(response.status(), StatusCode::OK, "{path} opens");
    response.into_body().into_data_stream()
}

async fn read_until(stream: &mut BodyDataStream, text: &mut String, needle: &str) {
    use futures_util::StreamExt as _;

    while !text.contains(needle) {
        let chunk = stream
            .next()
            .await
            .unwrap_or_else(|| panic!("the stream ended before {needle:?}: {text}"))
            .expect("the stream reads");
        text.push_str(&String::from_utf8_lossy(&chunk));
    }
}

fn ids(text: &str) -> Vec<String> {
    text.split("\n\n")
        .filter(|block| block.contains("\nevent: "))
        .filter_map(|block| block.strip_prefix("id: "))
        .map(|rest| rest.lines().next().unwrap_or("").to_owned())
        .collect()
}

#[test]
fn the_stream_opens_with_the_current_cursor_so_a_reconnect_resumes_rather_than_restarts() {
    test_app::run(async {
        let app = TestApp::start();
        let session = eight_texts(&app).await;
        let last_device = *app
            .store_events(3, "adb.reachable")
            .await
            .last()
            .expect("events");
        for (path, cursor) in [
            ("/api/v1/events", last_device.to_string()),
            ("/api/v1/events?types=message", "8".to_owned()),
            (
                "/api/v1/events?types=device,message",
                format!("{last_device}:8"),
            ),
            (
                "/api/v1/events?types=message&types=device",
                format!("{last_device}:8"),
            ),
        ] {
            let mut stream = open(&app, path, &[["cookie", &session]]).await;
            let mut text = String::new();
            read_until(&mut stream, &mut text, "\n\n").await;
            assert_eq!(text, format!("retry: 3000\nid: {cursor}\n\n"), "{path}");
        }
        let bad = app
            .call(
                "GET",
                "/api/v1/events?types=device,foo",
                &[["cookie", &session]],
                None,
            )
            .await;
        assert_eq!(bad.status, StatusCode::BAD_REQUEST);
        assert_eq!(
            bad.json(),
            json!({ "error": "types must be device, message, or both, not \"device,foo\"" })
        );
    });
}

#[test]
fn attachments_come_with_their_content_type_and_scriptable_types_download_sandboxed() {
    test_app::run(async {
        let app = TestApp::start();
        let session = eight_texts(&app).await;
        let store = AttachmentStore::new(app.state.config().data_dir.join("attachments"));
        let png = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
        let html = b"<script>alert(1)</script>";
        let now = app.state.clock().now();
        let saved_png = store.save(&png, now).expect("the image is saved");
        let saved_html = store.save(html, now).expect("the page is saved");
        let mut mms = new_text(20, "");
        mms.provider = Provider::Mms;
        mms.attachments = vec![
            NewAttachment {
                content_type: "image/png".to_owned(),
                size: saved_png.size,
                sha256: saved_png.sha256,
                filename: Some("a b.png".to_owned()),
            },
            NewAttachment {
                content_type: "text/html".to_owned(),
                size: saved_html.size,
                sha256: saved_html.sha256,
                filename: None,
            },
        ];
        let stored = ingest(&app, 1, vec![mms]).await;
        assert_eq!(
            stored[0].attachments[0].url, "/api/v1/attachments/1",
            "a text lists where to fetch its parts"
        );
        let cookie = [["cookie", session.as_str()]];
        let image = app
            .call("GET", "/api/v1/attachments/1", &cookie, None)
            .await;
        assert_eq!(image.status, StatusCode::OK);
        assert_eq!(image.header("content-type").as_deref(), Some("image/png"));
        assert_eq!(
            image.header("content-disposition").as_deref(),
            Some("inline; filename=\"a_b.png\"")
        );
        assert_eq!(
            image.header("cache-control").as_deref(),
            Some("private, max-age=31536000, immutable")
        );
        assert_eq!(
            image.header("x-content-type-options").as_deref(),
            Some("nosniff")
        );
        assert_eq!(image.body.as_ref(), png);
        let page = app
            .call("GET", "/api/v1/attachments/2", &cookie, None)
            .await;
        assert_eq!(
            page.header("content-disposition").as_deref(),
            Some("attachment; filename=\"attachment-2\"")
        );
        assert_eq!(
            page.header("content-security-policy").as_deref(),
            Some("sandbox; default-src 'none'")
        );
        let missing = app
            .call("GET", "/api/v1/attachments/9", &cookie, None)
            .await;
        assert_eq!(missing.status, StatusCode::NOT_FOUND);
        assert_eq!(missing.json(), json!({ "error": "no attachment 9" }));
        let zero = app
            .call("GET", "/api/v1/attachments/0", &cookie, None)
            .await;
        assert_eq!(zero.status, StatusCode::UNPROCESSABLE_ENTITY);
    });
}

#[test]
fn the_stream_resumes_from_last_event_id_then_streams_new_texts_with_a_heartbeat() {
    test_app::run(async {
        let app = TestApp::start();
        let session = eight_texts(&app).await;
        let mut stream = open(
            &app,
            "/api/v1/events?types=message",
            &[["cookie", &session], ["last-event-id", "6"]],
        )
        .await;
        let mut text = String::new();
        read_until(&mut stream, &mut text, "retry: 3000\nid: 8\n\n").await;
        ingest(&app, 1, vec![new_text(12, "Hello again")]).await;
        read_until(&mut stream, &mut text, "id: 9\n").await;
        time::advance(Duration::from_secs(15)).await;
        read_until(&mut stream, &mut text, ": keep-alive\n\n").await;
        assert_eq!(ids(&text), ["7", "8", "9"]);
        assert!(text.contains("event: message\ndata: {"), "{text}");
        assert!(!text.contains("event: device"), "{text}");
        let last = text
            .split("\n\n")
            .find(|block| block.starts_with("id: 9\n"))
            .and_then(|block| block.split("data: ").nth(1))
            .expect("the new text's event");
        let message: Value = serde_json::from_str(last).expect("JSON");
        assert_eq!(message["seq"], 9);
        assert_eq!(message["body"], "Hello again");
        assert_eq!(message["number"]["e164"], NUMBER);
    });
}

#[test]
fn a_stream_of_both_kinds_uses_device_colon_seq_ids_and_resumes_both_without_gaps() {
    test_app::run(async {
        let app = TestApp::start();
        let session = eight_texts(&app).await;
        let last = *app
            .store_events(3, "adb.reachable")
            .await
            .last()
            .expect("events");
        let cookie = ["cookie", session.as_str()];
        let mut first = open(
            &app,
            &format!("/api/v1/events?types=device,message&after={}:7", last - 1),
            &[cookie],
        )
        .await;
        let mut text = String::new();
        read_until(&mut first, &mut text, "retry: 3000\n").await;
        assert_eq!(ids(&text), [format!("{last}:7"), format!("{last}:8")]);

        let resume = format!("{last}:8");
        let mut resumed = open(
            &app,
            "/api/v1/events?types=device,message",
            &[cookie, ["last-event-id", &resume]],
        )
        .await;
        let mut text = String::new();
        read_until(&mut resumed, &mut text, "retry: 3000\n").await;
        app.store_events(1, "adb.reachable").await;
        let recorded = app
            .state
            .db()
            .call(move |db| db.device_events().after(last, 10))
            .await
            .expect("the event reads");
        app.state.buses().publish_device_events(&recorded);
        ingest(&app, 1, vec![new_text(12, "After the reconnect")]).await;
        read_until(&mut resumed, &mut text, &format!("id: {}:9\n", last + 1)).await;
        assert_eq!(
            ids(&text),
            [format!("{}:8", last + 1), format!("{}:9", last + 1)]
        );

        let bare = (last - 1).to_string();
        let mut old = open(
            &app,
            "/api/v1/events?types=device,message",
            &[cookie, ["last-event-id", &bare]],
        )
        .await;
        let mut text = String::new();
        read_until(&mut old, &mut text, "retry: 3000\n").await;
        assert_eq!(
            ids(&text),
            [format!("{last}:9"), format!("{}:9", last + 1)],
            "a bare number resumes device events; texts start with the next new one"
        );

        let plain_cursor = last.to_string();
        let mut plain = open(
            &app,
            "/api/v1/events",
            &[cookie, ["last-event-id", &plain_cursor]],
        )
        .await;
        let mut text = String::new();
        read_until(&mut plain, &mut text, "retry: 3000\n").await;
        assert_eq!(ids(&text), [(last + 1).to_string()]);
    });
}

#[test]
fn the_api_docs_document_the_message_routes_with_examples() {
    test_app::run(async {
        let app = TestApp::start();
        let document = app.call("GET", "/api/docs/json", &[], None).await.json();
        let paths = document["paths"].as_object().expect("paths");
        for path in [
            "/api/v1/numbers/{key}/messages",
            "/api/v1/messages",
            "/api/v1/attachments/{id}",
            "/api/v1/events",
        ] {
            assert!(paths.contains_key(path), "{path}");
        }
        assert!(
            document["paths"]["/api/v1/messages"]
                .to_string()
                .contains("Is the couch still available?")
        );
    });
}
