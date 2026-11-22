//! The screen routes over a real socket, ported from the route parts of spareline-old's
//! `src/screens/routes.test.ts`: who may open a screen, the Origin check, and the close codes
//! the route passes on. Streaming a running session is the screens module's tests' business,
//! since it needs a scrcpy stand-in.

use axum::http::StatusCode;
use serde_json::json;
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::error::Error as WsError;
use tokio_tungstenite::tungstenite::http::{HeaderName, HeaderValue};

use crate::test_app::{self, TestApp};

const PATH: &str = "/api/v1/phones/SPARELINE00001/screen";

/// Serves the app on a loopback port and returns its `host:port`.
async fn serve(app: &TestApp) -> String {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a loopback port is free");
    let address = listener.local_addr().expect("a bound address").to_string();
    let router = app.router.clone();
    tokio::spawn(async move {
        axum::serve(listener, router)
            .await
            .expect("the server runs");
    });
    address
}

/// How a socket was closed.
#[derive(Debug, PartialEq, Eq)]
struct Closed {
    code: u16,
    reason: String,
}

/// Opens the screen socket with `headers`, and returns the close frame's code and reason, or
/// the HTTP status the upgrade was refused with.
async fn close_of(address: &str, headers: &[[&str; 2]]) -> Result<Closed, u16> {
    use futures_util::StreamExt as _;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;

    let mut request = format!("ws://{address}{PATH}")
        .into_client_request()
        .expect("a valid request");
    for [name, value] in headers {
        request.headers_mut().insert(
            HeaderName::from_bytes(name.as_bytes()).expect("a header name"),
            HeaderValue::from_str(value).expect("a header value"),
        );
    }
    let stream = TcpStream::connect(address)
        .await
        .expect("the server accepts");
    let (mut socket, _) = match tokio_tungstenite::client_async(request, stream).await {
        Ok(connected) => connected,
        Err(WsError::Http(response)) => return Err(response.status().as_u16()),
        Err(error) => panic!("the handshake failed: {error}"),
    };
    while let Some(message) = socket.next().await {
        if let Ok(Message::Close(frame)) = message {
            let frame = frame.expect("a close frame with a code");
            return Ok(Closed {
                code: u16::from(frame.code),
                reason: frame.reason.to_string(),
            });
        }
    }
    panic!("the socket ended without a close frame");
}

#[test]
fn a_page_from_another_site_is_refused_before_the_upgrade() {
    test_app::run(async {
        let app = TestApp::start();
        let session = app.alice().await;
        let cookie = session.as_str();
        let address = serve(&app).await;
        assert_eq!(
            close_of(
                &address,
                &[["cookie", cookie], ["origin", "https://evil.example"]]
            )
            .await,
            Err(403)
        );
        assert_eq!(
            close_of(
                &address,
                &[["cookie", cookie], ["origin", "http://127.0.0.1:1"]]
            )
            .await,
            Err(403),
            "the same host name on another port is another origin"
        );
        let refused = app
            .call(
                "GET",
                PATH,
                &[
                    ["cookie", cookie],
                    ["origin", "https://evil.example"],
                    ["host", "127.0.0.1:8740"],
                ],
                None,
            )
            .await;
        assert_eq!(refused.text(), "cross-origin WebSocket refused");
    });
}

#[test]
fn the_apps_own_pages_and_clients_without_origin_reach_the_screens() {
    test_app::run(async {
        let app = TestApp::start();
        let session = app.alice().await;
        let cookie = session.as_str();
        let address = serve(&app).await;
        let origin = format!("http://{address}");
        let offline = Ok(Closed {
            code: 4001,
            reason: "phone SPARELINE00001 is not online".to_owned(),
        });
        assert_eq!(
            close_of(&address, &[["cookie", cookie], ["origin", &origin]]).await,
            offline,
            "an offline phone's screen closes with 4001 and a reason"
        );
        assert_eq!(close_of(&address, &[["cookie", cookie]]).await, offline);
    });
}

#[test]
fn a_screen_needs_a_session_or_a_full_key() {
    test_app::run(async {
        let app = TestApp::start();
        let session = app.alice().await;
        let read = app.create_key(&session, "reader", "read").await;
        let address = serve(&app).await;
        assert_eq!(close_of(&address, &[]).await, Err(401));
        let bearer = format!("Bearer {read}");
        assert_eq!(
            close_of(&address, &[["authorization", &bearer]]).await,
            Err(403),
            "a read key can't open a socket that sends input"
        );
    });
}

#[test]
fn a_plain_get_without_an_upgrade_starts_nothing() {
    test_app::run(async {
        let app = TestApp::start();
        let session = app.alice().await;
        let response = app.call("GET", PATH, &[["cookie", &session]], None).await;
        assert!(!response.status.is_success(), "{}", response.status);
        let sessions = app
            .call("GET", "/api/v1/screens", &[["cookie", &session]], None)
            .await;
        assert_eq!(sessions.status, StatusCode::OK);
        assert_eq!(sessions.json(), json!({ "sessions": [] }));
    });
}

#[test]
fn viewers_arriving_while_the_server_shuts_down_are_closed_with_1001() {
    test_app::run(async {
        let app = TestApp::start();
        let session = app.alice().await;
        let cookie = session.as_str();
        let address = serve(&app).await;
        app.state.screens().shutdown().await;
        let closed = close_of(&address, &[["cookie", cookie]]).await;
        assert_eq!(closed.map(|closed| closed.code), Ok(1001));
    });
}
