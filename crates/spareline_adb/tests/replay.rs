//! Replays byte transcripts recorded from a real adb server and phone, both as recorded and one
//! byte at a time, and checks the bytes the client sends against the recording. The transcripts
//! come from spareline-old's `scripts/record-adb-fixtures.ts`, with the phone's serial replaced.

use std::fs;
use std::future::Future;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use spareline_adb::{AdbClient, AdbErrorKind, DeviceEntry, DeviceState, Endpoint};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::runtime::Builder;
use tokio::task::{self, JoinHandle};

const SERIAL: &str = "SPARELINE00001";

fn block_on<F: Future>(future: F) -> F::Output {
    Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime builds")
        .block_on(future)
}

enum Event {
    Client(Vec<u8>),
    Server(Vec<u8>),
}

fn load(name: &str) -> Vec<Vec<Event>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(format!("{name}.json"));
    let text = fs::read_to_string(&path).expect("the fixture is readable");
    let json: Value = serde_json::from_str(&text).expect("the fixture is JSON");
    let mut connections = Vec::new();
    for connection in json["connections"]
        .as_array()
        .expect("connections is a list")
    {
        let mut events = Vec::new();
        for event in connection
            .as_array()
            .expect("a connection is a list of events")
        {
            let bytes = decode_hex(event["hex"].as_str().expect("an event has hex bytes"));
            match event["from"].as_str().expect("an event names its sender") {
                "client" => events.push(Event::Client(bytes)),
                "server" => events.push(Event::Server(bytes)),
                other => panic!("unknown sender {other}"),
            }
        }
        connections.push(events);
    }
    connections
}

fn decode_hex(text: &str) -> Vec<u8> {
    let digits: Vec<u8> = text
        .bytes()
        .map(|digit| {
            let value = char::from(digit)
                .to_digit(16)
                .expect("fixture bytes are hex");
            u8::try_from(value).expect("a hex digit fits in a byte")
        })
        .collect();
    digits
        .chunks(2)
        .map(|pair| pair[0] << 4 | pair[1])
        .collect()
}

/// A fake adb server that plays a transcript back. Its task returns every mismatch between
/// what the client sent and what the recording expects.
struct Replay {
    endpoint: Endpoint,
    task: JoinHandle<Vec<String>>,
}

impl Replay {
    async fn start(name: &str, rechunk: Option<usize>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a loopback port is free");
        let port = listener
            .local_addr()
            .expect("the listener has an address")
            .port();
        let connections = load(name);
        let task = tokio::spawn(async move {
            let mut mismatches = Vec::new();
            for events in connections {
                let (stream, _) = listener.accept().await.expect("the client connects");
                play(stream, events, rechunk, &mut mismatches).await;
            }
            mismatches
        });
        Self {
            endpoint: Endpoint::new("127.0.0.1".to_owned(), port),
            task,
        }
    }

    fn client(&self) -> AdbClient {
        AdbClient::new(self.endpoint.clone())
    }

    async fn mismatches(self) -> Vec<String> {
        self.task.await.expect("the replay task finishes")
    }
}

async fn play(
    mut stream: TcpStream,
    events: Vec<Event>,
    rechunk: Option<usize>,
    mismatches: &mut Vec<String>,
) {
    for event in events {
        match event {
            Event::Client(expected) => {
                let mut got = vec![0; expected.len()];
                if stream.read_exact(&mut got).await.is_err() {
                    mismatches.push("client closed early".to_owned());
                    return;
                }
                if got != expected {
                    mismatches.push(format!(
                        "client sent {:?}, recorded {:?}",
                        String::from_utf8_lossy(&got),
                        String::from_utf8_lossy(&expected)
                    ));
                }
            }
            Event::Server(bytes) => {
                for chunk in bytes.chunks(rechunk.unwrap_or(bytes.len().max(1))) {
                    stream
                        .write_all(chunk)
                        .await
                        .expect("the client reads the reply");
                    stream.flush().await.expect("the reply flushes");
                    if rechunk.is_some() {
                        task::yield_now().await;
                    }
                }
            }
        }
    }
    stream.shutdown().await.expect("the replay closes cleanly");
}

const CHUNKINGS: [Option<usize>; 2] = [None, Some(1)];

#[test]
fn host_version() {
    for rechunk in CHUNKINGS {
        block_on(async {
            let replay = Replay::start("version", rechunk).await;
            assert_eq!(replay.client().version().await.unwrap(), 41);
            assert_eq!(replay.mismatches().await, Vec::<String>::new());
        });
    }
}

#[test]
fn host_devices_l() {
    for rechunk in CHUNKINGS {
        block_on(async {
            let replay = Replay::start("devices-l", rechunk).await;
            let devices = replay.client().list_devices().await.unwrap();
            assert_eq!(
                devices,
                vec![DeviceEntry {
                    serial: SERIAL.to_owned(),
                    state: DeviceState::Device,
                    raw_state: "device".to_owned(),
                    usb: Some("1-1".to_owned()),
                    product: Some("S6702X1".to_owned()),
                    model: Some("S6702X".to_owned()),
                    device: Some("S6702XA".to_owned()),
                    transport_id: Some(1),
                }]
            );
            assert_eq!(replay.mismatches().await, Vec::<String>::new());
        });
    }
}

#[test]
fn track_devices_through_a_transport_reconnect() {
    for rechunk in CHUNKINGS {
        block_on(async {
            let replay = Replay::start("track-devices-reconnect", rechunk).await;
            let mut tracker = replay.client().track_devices().await.unwrap();
            let mut seen = Vec::new();
            while let Some(list) = tracker.next().await.unwrap() {
                let states: Vec<String> = list
                    .iter()
                    .map(|device| {
                        format!("{}#{}", device.state.as_str(), device.transport_id.unwrap())
                    })
                    .collect();
                seen.push(if states.is_empty() {
                    "none".to_owned()
                } else {
                    states.join(",")
                });
            }
            assert_eq!(
                seen,
                [
                    "device#1",
                    "offline#1",
                    "none",
                    "offline#2",
                    "authorizing#2",
                    "offline#2",
                    "device#2"
                ]
            );
            assert_eq!(replay.mismatches().await, Vec::<String>::new());
        });
    }
}

#[test]
fn shell_v2_separates_stdout_stderr_and_the_exit_code() {
    for rechunk in CHUNKINGS {
        block_on(async {
            let replay = Replay::start("shell-v2", rechunk).await;
            let output = replay
                .client()
                .shell(SERIAL, "getprop ro.product.model; echo oops >&2; exit 3")
                .await
                .unwrap();
            assert_eq!(output.stdout_text(), "S6702X\n");
            assert_eq!(output.stderr_text(), "oops\n");
            assert_eq!(output.exit_code, 3);
            assert_eq!(replay.mismatches().await, Vec::<String>::new());
        });
    }
}

#[test]
fn exec_returns_raw_binary_stdout() {
    for rechunk in CHUNKINGS {
        block_on(async {
            let replay = Replay::start("exec-binary", rechunk).await;
            let bytes = replay
                .client()
                .exec(SERIAL, "head -c 64 /system/bin/toybox")
                .await
                .unwrap();
            assert_eq!(bytes.len(), 64);
            assert_eq!(bytes[..4], [0x7f, 0x45, 0x4c, 0x46]);
            assert_eq!(replay.mismatches().await, Vec::<String>::new());
        });
    }
}

#[test]
fn stream_services_are_duplex() {
    for rechunk in CHUNKINGS {
        block_on(async {
            let replay = Replay::start("stream-echo", rechunk).await;
            let mut stream = replay
                .client()
                .open_stream(SERIAL, "tcp:27183")
                .await
                .unwrap();
            stream.write_all(b"hello spareline").await.unwrap();
            let mut echoed = Vec::new();
            stream.read_to_end(&mut echoed).await.unwrap();
            assert_eq!(echoed, b"hello spareline");
            assert_eq!(replay.mismatches().await, Vec::<String>::new());
        });
    }
}

#[test]
fn sync_push_sends_send_data_in_64_kib_chunks_done_and_quit_byte_for_byte() {
    block_on(async {
        let replay = Replay::start("push", None).await;
        let data: Vec<u8> = (0..70_000_u32)
            .map(|index| u8::try_from(index % 251).unwrap())
            .collect();
        let modified = UNIX_EPOCH + Duration::from_hours(497_928);
        replay
            .client()
            .push(
                SERIAL,
                &data,
                "/data/local/tmp/sl-fixture.bin",
                0o644,
                modified,
            )
            .await
            .unwrap();
        assert_eq!(replay.mismatches().await, Vec::<String>::new());
    });
}

#[test]
fn sync_push_surfaces_the_phones_fail_message() {
    block_on(async {
        let replay = Replay::start("push-fail", None).await;
        let error = replay
            .client()
            .push(
                SERIAL,
                &[1, 2, 3],
                "/system/sl-denied.bin",
                0o644,
                SystemTime::now(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind(), AdbErrorKind::Fail);
        assert!(error.message().contains("Read-only file system"), "{error}");
        drop(replay);
    });
}

#[test]
fn an_unknown_serial_fails_with_the_servers_message() {
    block_on(async {
        let replay = Replay::start("fail-unknown-serial", None).await;
        let error = replay
            .client()
            .shell("NOSUCHSERIAL", "true")
            .await
            .unwrap_err();
        assert_eq!(error.kind(), AdbErrorKind::Fail);
        assert!(
            error.message().contains("device 'NOSUCHSERIAL' not found"),
            "{error}"
        );
        assert_eq!(replay.mismatches().await, Vec::<String>::new());
    });
}

#[test]
fn localabstract_to_a_missing_socket_fails() {
    block_on(async {
        let replay = Replay::start("fail-localabstract", None).await;
        let error = replay
            .client()
            .open_stream(SERIAL, "localabstract:sl-no-such-socket")
            .await
            .unwrap_err();
        assert_eq!(error.kind(), AdbErrorKind::Fail);
        assert_eq!(error.message(), "localabstract:sl-no-such-socket: closed");
        assert_eq!(replay.mismatches().await, Vec::<String>::new());
    });
}

/// A fake adb server that answers every connection the same way.
struct FakeServer {
    endpoint: Endpoint,
    task: JoinHandle<()>,
}

async fn server_that(answer: &'static [u8]) -> FakeServer {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a loopback port is free");
    let port = listener
        .local_addr()
        .expect("the listener has an address")
        .port();
    let task = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.expect("the client connects");
            let mut request = [0; 512];
            let mut seen = Vec::new();
            loop {
                let read = stream.read(&mut request).await.unwrap_or(0);
                if read == 0 {
                    break;
                }
                seen.extend_from_slice(&request[..read]);
                if answer.is_empty() {
                    continue;
                }
                if String::from_utf8_lossy(&seen).contains("shell")
                    || !answer.starts_with(b"OKAY\x01")
                {
                    stream
                        .write_all(answer)
                        .await
                        .expect("the client reads the answer");
                    if !answer.starts_with(b"OKAY\x01") {
                        stream.shutdown().await.expect("the server closes cleanly");
                        break;
                    }
                } else {
                    stream
                        .write_all(b"OKAY")
                        .await
                        .expect("the client reads OKAY");
                }
            }
        }
    });
    FakeServer {
        endpoint: Endpoint::new("127.0.0.1".to_owned(), port),
        task,
    }
}

#[test]
fn a_refused_connection_is_a_connect_error() {
    block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a loopback port is free");
        let port = listener
            .local_addr()
            .expect("the listener has an address")
            .port();
        drop(listener);
        let client = AdbClient::new(Endpoint::new("127.0.0.1".to_owned(), port));
        let error = client.version().await.unwrap_err();
        assert_eq!(error.kind(), AdbErrorKind::Connect);
        assert!(
            error
                .message()
                .starts_with("cannot reach the adb server at 127.0.0.1:"),
            "{error}"
        );
    });
}

#[test]
fn a_silent_server_times_out() {
    block_on(async {
        let FakeServer { endpoint, task } = server_that(b"").await;
        let client = AdbClient::new(endpoint).with_call_timeout(Duration::from_millis(100));
        assert_eq!(
            client.version().await.unwrap_err().kind(),
            AdbErrorKind::Timeout
        );
        task.abort();
    });
}

#[test]
fn a_connection_closed_mid_frame_is_reported_not_hung() {
    block_on(async {
        let FakeServer { endpoint, task } = server_that(b"OKAY00").await;
        let error = AdbClient::new(endpoint).version().await.unwrap_err();
        assert_eq!(error.kind(), AdbErrorKind::Closed);
        task.abort();
    });
}

#[test]
fn an_impossible_frame_length_is_a_protocol_error_not_a_hang() {
    block_on(async {
        let FakeServer { endpoint, task } = server_that(b"OKAY\x01\xff\xff\xff\xff").await;
        let client = AdbClient::new(endpoint).with_call_timeout(Duration::from_secs(2));
        let error = client.shell(SERIAL, "true").await.unwrap_err();
        assert_eq!(error.kind(), AdbErrorKind::Protocol, "{error}");
        task.abort();
    });
}

#[test]
fn garbage_instead_of_a_status_is_a_protocol_error() {
    block_on(async {
        let FakeServer { endpoint, task } = server_that(b"HTTP/1.1 400 Bad Request\r\n\r\n").await;
        let error = AdbClient::new(endpoint).version().await.unwrap_err();
        assert_eq!(error.kind(), AdbErrorKind::Protocol);
        task.abort();
    });
}
