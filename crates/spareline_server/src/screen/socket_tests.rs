//! The screen WebSocket end to end: a real axum server whose route hands each socket to
//! [`Screens::attach`], a real WebSocket client, and the stand-in phone, as spareline-old's
//! `src/screens/routes.test.ts` drove them. The guard and the Origin check belong to the HTTP
//! routes and are tested there.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use std::{fs, str};

use axum::extract::{Path, WebSocketUpgrade};
use axum::response::Response;
use axum::{Router, routing};
use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::Value;
use spareline_core::screen::{MAX_TEXT_BYTES, MAX_VIEWER_MESSAGE_BYTES};
use tokio::net::{TcpListener, TcpSocket, TcpStream};
use tokio::sync::watch::{self, Receiver, Sender};
use tokio::time::{self, Instant};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;

use crate::fake_adb::{self, B30};
use crate::screen::screen_harness::{self, Harness};
use crate::screen::screen_session::ScreenState;
use crate::screen::screens::{SCREEN_SOCKET_LIMIT, Screens};

/// The signed-in viewer every socket of these tests belongs to.
const LOGIN: &str = "someone@example.com";

/// A running test server.
struct TestServer {
    port: u16,
    revoke: Sender<bool>,
    /// How many attaches have returned.
    finished: Receiver<usize>,
}

/// A server whose sockets send each write at once, as the stand-in phone's do: under Nagle's
/// algorithm a ping written after a frame waits for an acknowledgement in real time, which
/// tokio's paused clock outruns. Its `/screen/:serial` route attaches each socket for [`LOGIN`], revoked when the
/// returned sender says so.
async fn serve(screens: &Screens) -> TestServer {
    use axum::serve::ListenerExt as _;

    let (revoke, revoked) = watch::channel(false);
    let (finish, finished) = watch::channel(0);
    let screens = screens.clone();
    let route = routing::get(
        move |Path(serial): Path<String>, upgrade: WebSocketUpgrade| {
            let screens = screens.clone();
            let mut revoked = revoked.clone();
            let finish = finish.clone();
            async move {
                let upgrade = upgrade
                    .max_message_size(SCREEN_SOCKET_LIMIT)
                    .max_frame_size(SCREEN_SOCKET_LIMIT);
                let response: Response = upgrade.on_upgrade(move |socket| async move {
                    let revoked = async move {
                        match revoked.wait_for(|revoked| *revoked).await {
                            Ok(_) | Err(_) => {}
                        }
                    };
                    screens
                        .attach(&serial, socket, LOGIN.to_owned(), revoked)
                        .await;
                    finish.send_modify(|finished| *finished += 1);
                });
                response
            }
        },
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = Router::new().route("/screen/{serial}", route);
    let listener = listener.tap_io(|stream| {
        stream
            .set_nodelay(true)
            .expect("loopback sockets take TCP_NODELAY");
    });
    drop(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    }));
    TestServer {
        port,
        revoke,
        finished,
    }
}

type Client = WebSocketStream<TcpStream>;

async fn connect(port: u16, serial: &str) -> Client {
    let stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    stream.set_nodelay(true).unwrap();
    let (client, _response) =
        tokio_tungstenite::client_async(format!("ws://127.0.0.1:{port}/screen/{serial}"), stream)
            .await
            .unwrap();
    client
}

/// What a client received: text frames as JSON, binary frames, and the close.
#[derive(Default)]
struct Received {
    text: Vec<Value>,
    binary: Vec<Vec<u8>>,
    close: Option<CloseFrame>,
    ended: bool,
}

/// Reads the next frame into `received`.
async fn read(client: &mut Client, received: &mut Received) {
    match client.next().await {
        Some(Ok(Message::Text(text))) => received.text.push(serde_json::from_str(&text).unwrap()),
        Some(Ok(Message::Binary(bytes))) => received.binary.push(bytes.to_vec()),
        Some(Ok(Message::Close(close))) => {
            received.close = close;
            received.ended = true;
        }
        Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => {}
        Some(Err(_)) | None => received.ended = true,
    }
}

/// Reads frames until `done` holds or the socket ends.
async fn read_until(
    client: &mut Client,
    received: &mut Received,
    done: impl Fn(&Received) -> bool,
) {
    while !done(received) && !received.ended {
        read(client, received).await;
    }
}

#[test]
fn a_viewer_gets_hello_then_scrcpy_packets_as_binary_frames_and_only_allowed_input_goes_on() {
    fake_adb::block_on(async {
        let h = Harness::start(screen_harness::test_timings()).await;
        let server = serve(&h.screens).await;
        let port = server.port;
        let mut client = connect(port, B30).await;
        let mut received = Received::default();
        read_until(&mut client, &mut received, |r| r.binary.len() > 2).await;
        assert!(
            received
                .text
                .iter()
                .any(|m| m["t"] == "hello" && m["width"] == 216 && m["height"] == 480)
        );
        assert_eq!(
            received.binary[0][0], 0x80,
            "the first binary frame is the config packet"
        );

        client.send(Message::Text("ignored".into())).await.unwrap();
        client
            .send(Message::Binary(vec![9, 0].into()))
            .await
            .unwrap();
        client
            .send(Message::Binary(vec![10, 1].into()))
            .await
            .unwrap();
        client
            .send(Message::Binary(vec![4, 0].into()))
            .await
            .unwrap();
        fake_adb::until("the back press reaches the server", || {
            h.phone.servers()[0].control.contains(&vec![4, 0])
        })
        .await;
        let control = h.phone.servers()[0].control.clone();
        assert!(!control.contains(&vec![9, 0]));
        assert!(!control.contains(&vec![10, 1]));
        let list = h.screens.list();
        assert_eq!(list[0].state, ScreenState::Running);
        assert_eq!(list[0].viewers, 1);

        client.close(None).await.unwrap();
        fake_adb::until("the audit row closes", || {
            h.audits().iter().all(|row| row.ends_with("closed"))
        })
        .await;
        assert_eq!(h.audits(), vec![format!("{LOGIN} 1 0 2 closed")]);
        h.screens.shutdown().await;
    });
}

#[test]
fn a_viewer_message_over_4_kib_closes_the_socket_with_1009_under_the_routes_real_limits() {
    fake_adb::block_on(async {
        let h = Harness::start(screen_harness::test_timings()).await;
        let server = serve(&h.screens).await;
        for size in [MAX_VIEWER_MESSAGE_BYTES + 1, SCREEN_SOCKET_LIMIT] {
            let mut client = connect(server.port, B30).await;
            let mut received = Received::default();
            read_until(&mut client, &mut received, |r| {
                r.text.iter().any(|m| m["t"] == "hello")
            })
            .await;
            client
                .send(Message::Binary(vec![0; size].into()))
                .await
                .unwrap();
            read_until(&mut client, &mut received, |_| false).await;
            let close = received.close.expect("a close frame");
            assert_eq!(u16::from(close.code), 1009, "a message of {size} bytes");
            assert_eq!(close.reason.as_str(), "message too big");
        }
        fake_adb::until("the audit rows close", || {
            h.audits().iter().all(|row| row.ends_with("closed"))
        })
        .await;
        h.screens.shutdown().await;
    });
}

#[test]
fn a_client_that_answers_pings_stays_past_the_idle_time() {
    fake_adb::block_on(async {
        let h = Harness::start(screen_harness::test_timings()).await;
        let server = serve(&h.screens).await;
        let mut client = connect(server.port, B30).await;
        let mut received = Received::default();
        read_until(&mut client, &mut received, |r| !r.binary.is_empty()).await;
        let pings = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&pings);
        let reader = tokio::spawn(async move {
            while let Some(Ok(message)) = client.next().await {
                if let Message::Ping(_) = message {
                    counted.fetch_add(1, Ordering::AcqRel);
                }
            }
        });
        time::sleep(Duration::from_secs(155)).await;
        assert_eq!(pings.load(Ordering::Acquire), 5, "a ping every 30 s");
        assert_eq!(*server.finished.borrow(), 0, "the viewer is still attached");
        assert_eq!(h.screens.list()[0].viewers, 1);
        h.screens.shutdown().await;
        reader.await.unwrap();
    });
}

#[test]
fn a_client_that_goes_silent_is_dropped_after_120_s() {
    fake_adb::block_on(async {
        let h = Harness::start(screen_harness::test_timings()).await;
        let server = serve(&h.screens).await;
        let mut client = connect(server.port, B30).await;
        let mut received = Received::default();
        read_until(&mut client, &mut received, |r| !r.binary.is_empty()).await;
        let began = Instant::now();
        let mut finished = server.finished.clone();
        time::timeout(
            Duration::from_secs(300),
            finished.wait_for(|finished| *finished == 1),
        )
        .await
        .expect("the attach returns")
        .unwrap();
        let silent = began.elapsed();
        assert!(silent >= Duration::from_secs(119), "{silent:?}");
        assert!(silent <= Duration::from_secs(121), "{silent:?}");
        fake_adb::until("the audit row closes", || {
            h.audits().iter().all(|row| row.ends_with("closed"))
        })
        .await;
        fake_adb::until("the viewer has left", || {
            h.screens
                .list()
                .first()
                .is_none_or(|session| session.viewers == 0)
        })
        .await;
        drop(client);
        h.screens.shutdown().await;
    });
}

#[test]
fn a_revoked_viewer_whose_client_stopped_reading_is_dropped_within_the_close_timeout() {
    fake_adb::block_on(async {
        let h = Harness::start(screen_harness::test_timings()).await;
        let server = serve(&h.screens).await;
        let socket = TcpSocket::new_v4().unwrap();
        socket.set_recv_buffer_size(4096).unwrap();
        let stream = socket
            .connect(SocketAddr::from(([127, 0, 0, 1], server.port)))
            .await
            .unwrap();
        let (mut client, _response) = tokio_tungstenite::client_async(
            format!("ws://127.0.0.1:{}/screen/{B30}", server.port),
            stream,
        )
        .await
        .unwrap();
        let mut received = Received::default();
        read_until(&mut client, &mut received, |r| r.binary.len() >= 2).await;
        h.phone.send_packet(0, 0, 6_000_000);
        time::sleep(Duration::from_millis(200)).await;
        let began = Instant::now();
        server.revoke.send_replace(true);
        let mut finished = server.finished.clone();
        time::timeout(
            Duration::from_secs(300),
            finished.wait_for(|finished| *finished == 1),
        )
        .await
        .expect("the attach returns")
        .unwrap();
        let held = began.elapsed();
        assert!(
            held >= Duration::from_secs(5),
            "the close waited on the stalled client: {held:?}"
        );
        assert!(
            held < Duration::from_secs(6),
            "then the socket was dropped: {held:?}"
        );
        fake_adb::until("the audit row closes", || {
            h.audits().iter().all(|row| row.ends_with("closed"))
        })
        .await;
        drop(client);
        h.screens.shutdown().await;
    });
}

#[test]
fn viewers_arriving_while_the_server_shuts_down_are_closed_with_1001() {
    fake_adb::block_on(async {
        let h = Harness::start(screen_harness::test_timings()).await;
        let server = serve(&h.screens).await;
        let port = server.port;
        h.screens.shutdown().await;
        let mut client = connect(port, B30).await;
        let mut received = Received::default();
        read_until(&mut client, &mut received, |_| false).await;
        let close = received.close.expect("a close frame");
        assert_eq!(u16::from(close.code), 1001);
        assert_eq!(close.reason.as_str(), "the server is shutting down");
    });
}

#[test]
fn a_watched_phone_closes_its_viewers_with_1001_at_shutdown() {
    fake_adb::block_on(async {
        let h = Harness::start(screen_harness::test_timings()).await;
        let server = serve(&h.screens).await;
        let port = server.port;
        let mut client = connect(port, B30).await;
        let mut received = Received::default();
        read_until(&mut client, &mut received, |r| !r.binary.is_empty()).await;
        h.screens.shutdown().await;
        read_until(&mut client, &mut received, |_| false).await;
        let close = received.close.expect("a close frame");
        assert_eq!(u16::from(close.code), 1001);
        assert_eq!(close.reason.as_str(), "server shutting down");
    });
}

#[test]
fn an_offline_phones_screen_closes_with_4001_and_a_reason() {
    fake_adb::block_on(async {
        let h = Harness::start(screen_harness::test_timings()).await;
        let server = serve(&h.screens).await;
        let port = server.port;
        let mut client = connect(port, "NOTPLUGGEDIN").await;
        let mut received = Received::default();
        read_until(&mut client, &mut received, |_| false).await;
        let close = received.close.expect("a close frame");
        assert_eq!(u16::from(close.code), 4001);
        assert_eq!(close.reason.as_str(), "phone NOTPLUGGEDIN is not online");
        assert!(h.phone.servers().is_empty());
        h.screens.shutdown().await;
    });
}

#[test]
fn unplugging_a_watched_phone_closes_its_socket_with_4001() {
    fake_adb::block_on(async {
        let h = Harness::start(screen_harness::test_timings()).await;
        let server = serve(&h.screens).await;
        let port = server.port;
        let mut client = connect(port, B30).await;
        let mut received = Received::default();
        read_until(&mut client, &mut received, |r| !r.binary.is_empty()).await;
        h.unplug();
        read_until(&mut client, &mut received, |_| false).await;
        let close = received.close.expect("a close frame");
        assert_eq!(u16::from(close.code), 4001);
        assert_eq!(close.reason.as_str(), "phone offline");
        h.screens.shutdown().await;
    });
}

#[test]
fn a_viewer_whose_principal_is_revoked_is_closed_with_4003() {
    fake_adb::block_on(async {
        let h = Harness::start(screen_harness::test_timings()).await;
        let server = serve(&h.screens).await;
        let port = server.port;
        let mut client = connect(port, B30).await;
        let mut received = Received::default();
        read_until(&mut client, &mut received, |r| {
            r.text.iter().any(|m| m["t"] == "hello")
        })
        .await;
        server.revoke.send_replace(true);
        read_until(&mut client, &mut received, |_| false).await;
        let close = received.close.expect("a close frame");
        assert_eq!(u16::from(close.code), 4003);
        assert_eq!(close.reason.as_str(), "signed out");
        fake_adb::until("the audit row closes", || {
            h.audits().iter().all(|row| row.ends_with("closed"))
        })
        .await;
        fake_adb::until("the viewer has left", || h.screens.list()[0].viewers == 0).await;
        h.screens.shutdown().await;
    });
}

/// The web UI splits typed text into messages of at most its `MAX_TEXT_BYTES`, which must stay
/// the relay's limit, or the relay refuses every long paste.
#[test]
fn the_web_uis_text_split_stays_at_the_relays_text_limit() {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../spareline/web/src/screen-stream.ts");
    let source = fs::read_to_string(&path).unwrap();
    let declared: Vec<usize> = source
        .lines()
        .filter_map(|line| line.trim().strip_prefix("export const MAX_TEXT_BYTES = "))
        .map(|value| value.trim_end_matches(';').parse().unwrap())
        .collect();
    assert_eq!(declared, vec![MAX_TEXT_BYTES], "{}", path.display());
}
