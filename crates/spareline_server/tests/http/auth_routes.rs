//! Who may call what: the guard, sign-in, sessions, passwords and API keys, ported from
//! spareline-old's `src/auth/routes.test.ts`. No phone is attached.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::Response;
use futures_util::future;
use serde_json::{Value, json};
use spareline_server::api_docs;
use tokio::runtime::Builder;
use tokio::task::JoinSet;
use tokio::time;

use crate::test_app::{self, PASSWORD, TestApp};

/// Every route of the API, as `[method, path]`, with `WS` for the screen's WebSocket.
const ROUTES: [[&str; 2]; 27] = [
    ["GET", "/healthz"],
    ["GET", "/api/v1/health"],
    ["GET", "/api/v1/phones"],
    ["GET", "/api/v1/phones/{serial}"],
    ["PATCH", "/api/v1/phones/{serial}"],
    ["GET", "/api/v1/sims"],
    ["GET", "/api/v1/sims/{key}"],
    ["PATCH", "/api/v1/sims/{key}"],
    ["GET", "/api/v1/numbers"],
    ["GET", "/api/v1/numbers/{key}"],
    ["PATCH", "/api/v1/numbers/{key}"],
    ["POST", "/api/v1/auth/login"],
    ["GET", "/api/v1/auth/me"],
    ["POST", "/api/v1/auth/logout"],
    ["POST", "/api/v1/auth/password"],
    ["GET", "/api/v1/api-keys/"],
    ["POST", "/api/v1/api-keys/"],
    ["DELETE", "/api/v1/api-keys/{id}"],
    ["GET", "/api/v1/status"],
    ["POST", "/api/v1/phones/{serial}/provision"],
    ["POST", "/api/v1/phones/{serial}/check"],
    ["GET", "/api/v1/numbers/{key}/messages"],
    ["GET", "/api/v1/messages"],
    ["GET", "/api/v1/attachments/{id}"],
    ["GET", "/api/v1/events"],
    ["GET", "/api/v1/screens"],
    ["WS", "/api/v1/phones/{serial}/screen"],
];

/// Paths anyone may call.
const OPEN: [&str; 3] = ["/healthz", "/api/v1/auth/login", "/api/v1/auth/me"];

/// The headers of a WebSocket upgrade.
const UPGRADE: [[&str; 2]; 4] = [
    ["connection", "Upgrade"],
    ["upgrade", "websocket"],
    ["sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ=="],
    ["sec-websocket-version", "13"],
];

/// `path` with every `{param}` replaced by `x`.
fn concrete(path: &str) -> String {
    let mut out = String::new();
    let mut in_param = false;
    for c in path.chars() {
        match c {
            '{' => {
                in_param = true;
                out.push('x');
            }
            '}' => in_param = false,
            _ if in_param => {}
            _ => out.push(c),
        }
    }
    out
}

/// Calls the route `[method, path]` with `headers`, as a WebSocket upgrade for `WS`.
async fn call_route(app: &TestApp, route: [&str; 2], headers: &[[&str; 2]]) -> StatusCode {
    let [method, path] = route;
    let path = concrete(path);
    let mut all: Vec<[&str; 2]> = headers.to_vec();
    if method == "WS" {
        all.extend(UPGRADE);
    } else {
        all.push(["content-type", "application/json"]);
    }
    let method = if method == "WS" { "GET" } else { method };
    app.send(method, &path, &all, None).await.status()
}

#[test]
fn the_route_list_is_every_documented_route() {
    let document: Value =
        serde_json::from_str(&api_docs::openapi_document("test")).expect("the document is JSON");
    let documented: BTreeSet<String> = document["paths"]
        .as_object()
        .expect("paths")
        .iter()
        .flat_map(|(path, methods)| {
            methods
                .as_object()
                .expect("methods")
                .keys()
                .map(move |method| format!("{} {path}", method.to_uppercase()))
        })
        .collect();
    let listed: BTreeSet<String> = ROUTES
        .iter()
        .map(|[method, path]| format!("{method} {path}"))
        .collect();
    assert_eq!(listed, documented, "ROUTES lists every documented route");
}

#[test]
fn every_api_route_but_sign_in_who_am_i_and_the_docs_refuses_a_request_without_a_session() {
    test_app::run(async {
        let app = TestApp::start();
        app.add_user("alice");
        let mut checked = 0;
        for route in ROUTES {
            if OPEN.contains(&route[1]) {
                continue;
            }
            let status = call_route(&app, route, &[]).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{route:?}");
            checked += 1;
        }
        assert!(
            checked > 20,
            "phones, SIMs, numbers, texts, events, screens, status, set-up"
        );
        let unknown = app.call("GET", "/api/v1/nothing", &[], None).await;
        assert_eq!(
            unknown.status,
            StatusCode::UNAUTHORIZED,
            "the guard runs before routing"
        );
        let preflight = app.call("OPTIONS", "/api/v1/phones", &[], None).await;
        assert_eq!(
            preflight.status,
            StatusCode::UNAUTHORIZED,
            "no CORS preflight is granted"
        );
        assert_eq!(preflight.header("access-control-allow-origin"), None);
        assert_eq!(
            app.call("GET", "/healthz", &[], None).await.status,
            StatusCode::OK
        );
        assert_eq!(
            app.call("GET", "/api/docs/json", &[], None).await.status,
            StatusCode::OK
        );
        assert_eq!(
            app.call("GET", "/api/docs", &[], None).await.status,
            StatusCode::OK
        );
        assert_eq!(
            app.call("GET", "/numbers/1", &[], None).await.status,
            StatusCode::OK
        );
    });
}

#[test]
fn signing_in_sets_an_http_only_same_site_strict_cookie_that_opens_the_api() {
    test_app::run(async {
        let app = TestApp::start();
        app.add_user("alice");
        let response = app.sign_in("alice", PASSWORD).await;
        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(response.json(), json!({ "user": { "name": "alice" } }));
        assert_eq!(
            response.header("content-type").as_deref(),
            Some("application/json")
        );
        let cookie = response.header("set-cookie").expect("a cookie");
        let session = response.session();
        assert_eq!(
            cookie,
            format!("{session}; Path=/; HttpOnly; SameSite=Strict; Max-Age=2592000"),
            "no Secure flag over plain HTTP, or the browser would drop the cookie"
        );
        assert_eq!(session.len(), "spareline_session=".len() + 43);
        let phones = app
            .call("GET", "/api/v1/phones", &[["cookie", &session]], None)
            .await;
        assert_eq!(phones.status, StatusCode::OK);
        let me = app
            .call("GET", "/api/v1/auth/me", &[["cookie", &session]], None)
            .await;
        assert_eq!(me.json(), json!({ "user": { "name": "alice" } }));
        let upper = app.sign_in("ALICE", PASSWORD).await;
        assert_eq!(
            upper.json(),
            json!({ "user": { "name": "alice" } }),
            "the name matches in any case and the stored case comes back"
        );
    });
}

#[test]
fn behind_an_https_proxy_the_cookie_is_secure() {
    test_app::run(async {
        let app = TestApp::start();
        app.add_user("alice");
        let body = json!({ "name": "alice", "password": PASSWORD }).to_string();
        let response = app
            .call(
                "POST",
                "/api/v1/auth/login",
                &[["x-forwarded-proto", "https"]],
                Some(&body),
            )
            .await;
        let cookie = response.header("set-cookie").expect("a cookie");
        assert!(cookie.ends_with("; Secure"), "{cookie}");
    });
}

#[test]
fn a_wrong_password_an_unknown_name_and_a_form_post_are_refused_alike() {
    test_app::run(async {
        let app = TestApp::start();
        app.add_user("alice");
        for [name, password] in [["alice", "wrong password"], ["mallory", PASSWORD]] {
            let response = app.sign_in(name, password).await;
            assert_eq!(response.status, StatusCode::UNAUTHORIZED, "{name}");
            assert_eq!(
                response.json(),
                json!({ "error": "wrong name or password" })
            );
            assert_eq!(response.header("set-cookie"), None);
        }
        let form = app
            .call(
                "POST",
                "/api/v1/auth/login",
                &[["content-type", "application/x-www-form-urlencoded"]],
                None,
            )
            .await;
        assert_eq!(form.status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert_eq!(
            form.json(),
            json!({ "error": "send Content-Type: application/json" })
        );
        assert_eq!(form.header("set-cookie"), None);
        let empty = app
            .call(
                "POST",
                "/api/v1/auth/login",
                &[],
                Some(r#"{"name":"","password":"x"}"#),
            )
            .await;
        assert_eq!(empty.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            empty.json(),
            json!({
                "error": "name: expected a string of at least 1 characters, got 0 at line 1 column 10"
            })
        );
    });
}

#[test]
fn ten_failed_sign_ins_lock_the_name_out_for_a_while_even_with_the_right_password() {
    test_app::run(async {
        let app = TestApp::start();
        app.add_user("alice");
        for _ in 0..10 {
            assert_eq!(
                app.sign_in("alice", "wrong").await.status,
                StatusCode::UNAUTHORIZED
            );
        }
        let locked = app.sign_in("alice", PASSWORD).await;
        assert_eq!(locked.status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(locked.header("retry-after").as_deref(), Some("900"));
        assert_eq!(
            locked.json(),
            json!({ "error": "too many failed sign-ins; try again in 900 s" })
        );
        assert_eq!(locked.header("set-cookie"), None);
        time::advance(Duration::from_mins(15)).await;
        assert_eq!(
            app.sign_in("alice", PASSWORD).await.status,
            StatusCode::OK,
            "the oldest failure is 15 minutes old"
        );
    });
}

#[test]
fn a_name_with_a_byte_order_mark_shares_the_lockout_of_the_name_without_it() {
    test_app::run(async {
        let app = TestApp::start();
        app.add_user("alice");
        for _ in 0..10 {
            assert_eq!(
                app.sign_in("alice", "wrong").await.status,
                StatusCode::UNAUTHORIZED
            );
        }
        for spelling in ["\u{feff}alice", "alice\u{feff}", "\u{3000}alice"] {
            let locked = app.sign_in(spelling, PASSWORD).await;
            assert_eq!(
                locked.status,
                StatusCode::TOO_MANY_REQUESTS,
                "{spelling:?} reaches alice's account, so it shares her allowance"
            );
        }
        time::advance(Duration::from_mins(15)).await;
        let signed_in = app.sign_in("\u{feff}alice", PASSWORD).await;
        assert_eq!(signed_in.status, StatusCode::OK, "the spelling is alice");
        assert_eq!(signed_in.json(), json!({ "user": { "name": "alice" } }));
    });
}

#[test]
fn sign_ins_queue_for_as_many_places_as_the_hash_gate_lets_run() {
    test_app::run(async {
        let app = TestApp::start();
        assert_eq!(
            app.state.free_hash_permits(),
            spareline_db::max_concurrent_hashes(),
            "a place more than the gate has would hold a blocking thread waiting at the gate"
        );
    });
}

#[test]
fn the_database_answers_while_a_flood_of_sign_ins_waits_to_hash() {
    use tower::ServiceExt as _;

    const SIGN_INS: usize = 20;
    let runtime = Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(6)
        .build()
        .expect("a current-thread runtime builds");
    runtime.block_on(async {
        let app = TestApp::start();
        let session = app.alice().await;
        let finished = Arc::new(AtomicUsize::new(0));
        let mut sign_ins = JoinSet::new();
        for index in 0..SIGN_INS {
            let router = app.router.clone();
            let finished = Arc::clone(&finished);
            sign_ins.spawn(async move {
                let body = json!({ "name": format!("ghost{index}"), "password": "wrong" });
                let request = Request::post("/api/v1/auth/login")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .expect("the request is valid");
                let response = router.oneshot(request).await.expect("the router answers");
                finished.fetch_add(1, Ordering::SeqCst);
                response.status()
            });
        }
        let first = sign_ins.join_next().await.expect("a sign-in");
        assert_eq!(
            first.expect("the sign-in task ends"),
            StatusCode::UNAUTHORIZED
        );
        let phones = app
            .call("GET", "/api/v1/phones", &[["cookie", &session]], None)
            .await;
        let finished_then = finished.load(Ordering::SeqCst);
        assert_eq!(phones.status, StatusCode::OK);
        assert!(
            finished_then < SIGN_INS / 2,
            "the database answered only after {finished_then} of {SIGN_INS} unknown-name \
             sign-ins had hashed, so waiting hashes held its blocking threads"
        );
        while let Some(result) = sign_ins.join_next().await {
            assert_eq!(
                result.expect("the sign-in task ends"),
                StatusCode::UNAUTHORIZED
            );
        }
    });
}

#[test]
fn who_am_i_says_when_nobody_can_sign_in_yet() {
    test_app::run(async {
        let app = TestApp::start();
        let none = app.call("GET", "/api/v1/auth/me", &[], None).await;
        assert_eq!(none.status, StatusCode::UNAUTHORIZED);
        assert_eq!(
            none.json(),
            json!({
                "error": "no users yet: create one on the server with `spareline users add NAME`",
                "noUsers": true
            })
        );
        app.add_user("alice");
        let me = app.call("GET", "/api/v1/auth/me", &[], None).await;
        assert_eq!(
            me.json(),
            json!({ "error": "sign in first", "noUsers": false })
        );
    });
}

#[test]
fn signing_out_ends_the_session_and_clears_the_cookie() {
    test_app::run(async {
        let app = TestApp::start();
        let session = app.alice().await;
        let out = app
            .call(
                "POST",
                "/api/v1/auth/logout",
                &[["cookie", &session]],
                Some("{}"),
            )
            .await;
        assert_eq!(out.status, StatusCode::OK);
        assert_eq!(out.json(), json!({ "ok": true }));
        assert_eq!(
            out.header("set-cookie").as_deref(),
            Some("spareline_session=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0")
        );
        let after = app
            .call("GET", "/api/v1/phones", &[["cookie", &session]], None)
            .await;
        assert_eq!(after.status, StatusCode::UNAUTHORIZED);
        let again = app
            .call(
                "POST",
                "/api/v1/auth/logout",
                &[["cookie", &session]],
                Some("{}"),
            )
            .await;
        assert_eq!(
            again.status,
            StatusCode::UNAUTHORIZED,
            "the guard answers first"
        );
    });
}

#[test]
fn changing_the_password_needs_the_current_one_and_keeps_only_this_session() {
    test_app::run(async {
        let app = TestApp::start();
        let here = app.alice().await;
        let there = app.sign_in("alice", PASSWORD).await.session();
        let change = async |body: Value| {
            app.call(
                "POST",
                "/api/v1/auth/password",
                &[["cookie", &here]],
                Some(&body.to_string()),
            )
            .await
        };
        let wrong = change(json!({ "current": "wrong", "next": "a long new password" })).await;
        assert_eq!(wrong.status, StatusCode::FORBIDDEN);
        assert_eq!(
            wrong.json(),
            json!({ "error": "the current password is wrong" })
        );
        let short = change(json!({ "current": PASSWORD, "next": "short" })).await;
        assert_eq!(short.status, StatusCode::BAD_REQUEST);
        assert_eq!(
            short.json(),
            json!({ "error": "a password needs at least 12 characters" })
        );
        let changed = change(json!({ "current": PASSWORD, "next": "a long new password" })).await;
        assert_eq!(changed.status, StatusCode::OK);
        assert_eq!(changed.json(), json!({ "ok": true }));
        let here_after = app
            .call("GET", "/api/v1/phones", &[["cookie", &here]], None)
            .await;
        assert_eq!(here_after.status, StatusCode::OK);
        let there_after = app
            .call("GET", "/api/v1/phones", &[["cookie", &there]], None)
            .await;
        assert_eq!(there_after.status, StatusCode::UNAUTHORIZED);
        assert_eq!(
            app.sign_in("alice", "a long new password").await.status,
            StatusCode::OK
        );
    });
}

#[test]
fn concurrent_wrong_passwords_cannot_get_past_the_limit_and_one_name_does_not_lock_out_another() {
    test_app::run(async {
        let app = TestApp::start();
        app.add_user("alice");
        app.add_user("bob");
        let attempts = (0..30).map(|_| app.sign_in("alice", "wrong"));
        let results = future::join_all(attempts).await;
        let unauthorized = results
            .iter()
            .filter(|result| result.status == StatusCode::UNAUTHORIZED)
            .count();
        let limited = results
            .iter()
            .filter(|result| result.status == StatusCode::TOO_MANY_REQUESTS)
            .count();
        assert_eq!([unauthorized, limited], [10, 20]);
        assert_eq!(app.sign_in("bob", PASSWORD).await.status, StatusCode::OK);
    });
}

#[test]
fn the_current_password_check_is_rate_limited_too() {
    test_app::run(async {
        let app = TestApp::start();
        let session = app.alice().await;
        let body = json!({ "current": "wrong", "next": "a long new password" }).to_string();
        for _ in 0..10 {
            let response = app
                .call(
                    "POST",
                    "/api/v1/auth/password",
                    &[["cookie", &session]],
                    Some(&body),
                )
                .await;
            assert_eq!(response.status, StatusCode::FORBIDDEN);
        }
        let limited = app
            .call(
                "POST",
                "/api/v1/auth/password",
                &[["cookie", &session]],
                Some(&body),
            )
            .await;
        assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            limited.json(),
            json!({ "error": "too many failed attempts; try again in 900 s" })
        );
    });
}

#[test]
fn a_session_used_after_an_hour_gets_its_cookie_renewed_for_30_more_days() {
    test_app::run(async {
        let app = TestApp::start();
        let session = app.alice().await;
        let first = app
            .call("GET", "/api/v1/phones", &[["cookie", &session]], None)
            .await;
        assert_eq!(first.header("set-cookie"), None);
        time::advance(Duration::from_hours(2)).await;
        let later = app
            .call("GET", "/api/v1/phones", &[["cookie", &session]], None)
            .await;
        assert_eq!(later.status, StatusCode::OK);
        assert_eq!(
            later.header("set-cookie"),
            Some(format!(
                "{session}; Path=/; HttpOnly; SameSite=Strict; Max-Age=2592000"
            ))
        );
        let expires: String = app
            .sql
            .query_row("SELECT expires_at FROM sessions", [], |row| row.get(0))
            .expect("one session");
        assert_eq!(expires, "2026-12-22T02:00:00.000Z");
    });
}

/// Reads `body` until it ends, and says whether it did.
async fn ends(response: Response) -> bool {
    use futures_util::StreamExt as _;

    let mut stream = response.into_body().into_data_stream();
    while let Some(chunk) = stream.next().await {
        if chunk.is_err() {
            return true;
        }
    }
    true
}

#[test]
fn an_open_event_stream_ends_when_its_session_ends() {
    test_app::run(async {
        let app = TestApp::start();
        let session = app.alice().await;
        let stream = app
            .send(
                "GET",
                "/api/v1/events?types=device,message",
                &[["cookie", &session]],
                None,
            )
            .await;
        assert_eq!(stream.status(), StatusCode::OK);
        let ended = tokio::spawn(ends(stream));
        let logout = app
            .call(
                "POST",
                "/api/v1/auth/logout",
                &[["cookie", &session]],
                Some("{}"),
            )
            .await;
        assert_eq!(logout.status, StatusCode::OK);
        let done = time::timeout(Duration::from_secs(1), ended).await;
        assert!(
            matches!(done, Ok(Ok(true))),
            "the stream ends at once on the sign-out"
        );
    });
}

#[test]
fn a_session_removed_by_another_process_ends_open_streams_within_seconds() {
    test_app::run(async {
        let app = TestApp::start();
        let session = app.alice().await;
        let stream = app
            .send("GET", "/api/v1/events", &[["cookie", &session]], None)
            .await;
        let ended = tokio::spawn(ends(stream));
        app.sql
            .execute("DELETE FROM sessions", [])
            .expect("the command line's change");
        let done = time::timeout(Duration::from_secs(6), ended).await;
        assert!(
            matches!(done, Ok(Ok(true))),
            "the 5 s check notices the change no event announced"
        );
    });
}

#[test]
fn an_api_key_is_shown_once_listed_only_by_its_prefix_and_opens_the_api() {
    test_app::run(async {
        let app = TestApp::start();
        let session = app.alice().await;
        let key = app.create_key(&session, "texts script", "full").await;
        assert!(key.starts_with("sl_") && key.len() == 46, "{key}");
        let list = app
            .call("GET", "/api/v1/api-keys", &[["cookie", &session]], None)
            .await;
        let keys = list.json()["keys"].clone();
        assert_eq!(keys[0]["name"], "texts script");
        assert_eq!(keys[0]["user"], "alice");
        assert_eq!(keys[0]["scope"], "full");
        assert_eq!(keys[0]["prefix"], key[..9]);
        assert_eq!(
            test_app::ordered_keys(&list.text(), &["keys", "0"]),
            [
                "id",
                "user",
                "name",
                "scope",
                "prefix",
                "createdAt",
                "lastUsedAt",
                "revokedAt"
            ]
        );
        assert!(
            !keys.to_string().contains(&key[9..]),
            "the key itself isn't listed"
        );
        let bearer = format!("Bearer {key}");
        for path in ["/api/v1/phones", "/api/v1/messages"] {
            let response = app
                .call("GET", path, &[["authorization", &bearer]], None)
                .await;
            assert_eq!(response.status, StatusCode::OK, "{path}");
        }
        let used: bool = app
            .sql
            .query_row("SELECT last_used_at IS NOT NULL FROM api_keys", [], |row| {
                row.get(0)
            })
            .expect("one key");
        assert!(used, "using the key records it");
        let trailing = app
            .call("GET", "/api/v1/api-keys/", &[["cookie", &session]], None)
            .await;
        assert_eq!(
            trailing.status,
            StatusCode::OK,
            "a trailing slash is ignored"
        );
    });
}

#[test]
fn a_read_key_may_only_read_and_neither_kind_may_manage_keys_or_passwords() {
    test_app::run(async {
        let app = TestApp::start();
        let session = app.alice().await;
        let read = app.create_key(&session, "reader", "read").await;
        let full = app.create_key(&session, "writer", "full").await;
        let read_bearer = format!("Bearer {read}");
        for route in ROUTES {
            let [method, path] = route;
            if !path.starts_with("/api/v1/") || path.starts_with("/api/v1/auth/") {
                continue;
            }
            let status = call_route(&app, route, &[["authorization", &read_bearer]]).await;
            let refused = method != "GET" || path.starts_with("/api/v1/api-keys");
            assert_eq!(
                status == StatusCode::FORBIDDEN,
                refused,
                "{route:?}: {status}"
            );
        }
        for key in [&read, &full] {
            let bearer = format!("Bearer {key}");
            let mint = app
                .call(
                    "POST",
                    "/api/v1/api-keys",
                    &[["authorization", &bearer]],
                    Some(r#"{"name":"more","scope":"full"}"#),
                )
                .await;
            assert_eq!(mint.status, StatusCode::FORBIDDEN);
            assert_eq!(
                mint.json(),
                json!({ "error": "API keys cannot manage keys or passwords; sign in with a browser" })
            );
            let password = app
                .call(
                    "POST",
                    "/api/v1/auth/password",
                    &[["authorization", &bearer]],
                    Some(r#"{"current":"x","next":"a long new password"}"#),
                )
                .await;
            assert_eq!(password.status, StatusCode::FORBIDDEN);
            let me = app
                .call(
                    "GET",
                    "/api/v1/auth/me",
                    &[["authorization", &bearer]],
                    None,
                )
                .await;
            assert_eq!(me.status, StatusCode::FORBIDDEN, "even who-am-I");
        }
        let read_edit = app
            .call(
                "PATCH",
                "/api/v1/numbers/1",
                &[["authorization", &read_bearer]],
                Some(r#"{"note":"x"}"#),
            )
            .await;
        assert_eq!(
            read_edit.json(),
            json!({ "error": "this API key can only read" })
        );
        let full_bearer = format!("Bearer {full}");
        let edit = app
            .call(
                "PATCH",
                "/api/v1/numbers/1",
                &[["authorization", &full_bearer]],
                Some(r#"{"note":"x"}"#),
            )
            .await;
        assert_eq!(
            edit.status,
            StatusCode::NOT_FOUND,
            "a full key may change things"
        );
    });
}

#[test]
fn a_revoked_or_unknown_key_is_refused_even_next_to_a_valid_session_cookie() {
    test_app::run(async {
        let app = TestApp::start();
        let session = app.alice().await;
        let key = app.create_key(&session, "old", "full").await;
        let id: i64 = app
            .sql
            .query_row("SELECT id FROM api_keys", [], |row| row.get(0))
            .expect("one key");
        let revoke = app
            .call(
                "DELETE",
                &format!("/api/v1/api-keys/{id}"),
                &[["cookie", &session]],
                None,
            )
            .await;
        assert_eq!(revoke.status, StatusCode::OK);
        assert_eq!(revoke.json()["name"], "old");
        let revoked_at = revoke.json()["revokedAt"].clone();
        assert!(revoked_at.is_string(), "{revoked_at}");
        time::advance(Duration::from_secs(60)).await;
        let again = app
            .call(
                "DELETE",
                &format!("/api/v1/api-keys/{id}"),
                &[["cookie", &session]],
                None,
            )
            .await;
        assert_eq!(
            again.json()["revokedAt"],
            revoked_at,
            "revoking is idempotent"
        );
        let bearer = format!("Bearer {key}");
        let refused = app
            .call(
                "GET",
                "/api/v1/phones",
                &[["authorization", &bearer], ["cookie", &session]],
                None,
            )
            .await;
        assert_eq!(refused.status, StatusCode::UNAUTHORIZED);
        assert_eq!(
            refused.json(),
            json!({ "error": "unknown or revoked API key" })
        );
        let unknown = app
            .call(
                "GET",
                "/api/v1/phones",
                &[["authorization", "Bearer sl_nope"]],
                None,
            )
            .await;
        assert_eq!(unknown.status, StatusCode::UNAUTHORIZED);
        let missing = app
            .call(
                "DELETE",
                "/api/v1/api-keys/999",
                &[["cookie", &session]],
                None,
            )
            .await;
        assert_eq!(missing.status, StatusCode::NOT_FOUND);
        assert_eq!(missing.json(), json!({ "error": "no API key 999" }));
        let malformed = app
            .call(
                "DELETE",
                "/api/v1/api-keys/abc",
                &[["cookie", &session]],
                None,
            )
            .await;
        assert_eq!(malformed.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            malformed.json(),
            json!({ "error": "id: expected an integer, got \"abc\"" })
        );
    });
}

#[test]
fn a_key_name_is_one_to_64_characters_after_trimming() {
    test_app::run(async {
        let app = TestApp::start();
        let session = app.alice().await;
        let blank = app
            .call(
                "POST",
                "/api/v1/api-keys",
                &[["cookie", &session]],
                Some(r#"{"name":"   ","scope":"read"}"#),
            )
            .await;
        assert_eq!(blank.status, StatusCode::BAD_REQUEST);
        assert_eq!(
            blank.json(),
            json!({ "error": "a key name is 1-64 characters" })
        );
        let scope = app
            .call(
                "POST",
                "/api/v1/api-keys",
                &[["cookie", &session]],
                Some(r#"{"name":"x","scope":"admin"}"#),
            )
            .await;
        assert_eq!(scope.status, StatusCode::UNPROCESSABLE_ENTITY);
    });
}

#[test]
fn revoking_a_key_ends_its_open_event_stream_at_once() {
    test_app::run(async {
        let app = TestApp::start();
        let session = app.alice().await;
        let key = app.create_key(&session, "stream", "read").await;
        let bearer = format!("Bearer {key}");
        let stream = app
            .send(
                "GET",
                "/api/v1/events?types=message",
                &[["authorization", &bearer]],
                None,
            )
            .await;
        assert_eq!(stream.status(), StatusCode::OK);
        let ended = tokio::spawn(ends(stream));
        let id: i64 = app
            .sql
            .query_row("SELECT id FROM api_keys WHERE name = 'stream'", [], |row| {
                row.get(0)
            })
            .expect("the key");
        let revoke = app
            .call(
                "DELETE",
                &format!("/api/v1/api-keys/{id}"),
                &[["cookie", &session]],
                None,
            )
            .await;
        assert_eq!(revoke.status, StatusCode::OK);
        let done = time::timeout(Duration::from_secs(1), ended).await;
        assert!(matches!(done, Ok(Ok(true))), "the stream ends at once");
    });
}
