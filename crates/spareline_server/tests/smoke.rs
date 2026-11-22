//! The device smoke against a stand-in adb server, since a phone can't be plugged in on demand.
//! The stand-in speaks the same wire format the adb client's recorded transcripts show.

use std::future::Future;
use std::str;

use spareline_adb::{AdbClient, Endpoint};
use spareline_server::smoke::{self, Smoke, SmokeError};
use tokio::io::AsyncWriteExt as _;
use tokio::net::{TcpListener, TcpStream};
use tokio::runtime::Builder;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

fn block_on<F: Future>(future: F) -> F::Output {
    Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime builds")
        .block_on(future)
}

/// What the stand-in answers: the device list, and per serial the stdout and exit code of the
/// smoke's `getprop`.
#[derive(Clone)]
struct Phones {
    list: &'static str,
    answers: Vec<PhoneAnswer>,
}

#[derive(Clone)]
struct PhoneAnswer {
    serial: &'static str,
    stdout: &'static str,
    exit_code: u8,
}

async fn read_request(stream: &mut TcpStream) -> Option<String> {
    use tokio::io::AsyncReadExt as _;

    let mut length = [0; 4];
    stream.read_exact(&mut length).await.ok()?;
    let length = usize::from_str_radix(str::from_utf8(&length).ok()?, 16).ok()?;
    let mut body = vec![0; length];
    stream.read_exact(&mut body).await.ok()?;
    String::from_utf8(body).ok()
}

async fn hex_string(stream: &mut TcpStream, text: &str) {
    let framed = format!("OKAY{:04x}{text}", text.len());
    stream
        .write_all(framed.as_bytes())
        .await
        .expect("the client reads");
}

async fn serve(stream: &mut TcpStream, phones: &Phones, commands: &UnboundedSender<String>) {
    let Some(request) = read_request(stream).await else {
        return;
    };
    match request.as_str() {
        "host:version" => hex_string(stream, "0029").await,
        "host:devices-l" => hex_string(stream, phones.list).await,
        transport => {
            let serial = transport
                .strip_prefix("host:transport:")
                .expect("a transport request");
            stream.write_all(b"OKAY").await.expect("the client reads");
            let shell = read_request(stream).await.expect("a shell request");
            commands
                .send(shell)
                .expect("the test collects the commands");
            stream.write_all(b"OKAY").await.expect("the client reads");
            let answer = phones
                .answers
                .iter()
                .find(|answer| answer.serial == serial)
                .expect("the smoke reads only listed phones");
            let mut packets = vec![1];
            let stdout_length = u32::try_from(answer.stdout.len()).expect("short stdout");
            packets.extend_from_slice(&stdout_length.to_le_bytes());
            packets.extend_from_slice(answer.stdout.as_bytes());
            packets.extend_from_slice(&[3, 1, 0, 0, 0, answer.exit_code]);
            stream.write_all(&packets).await.expect("the client reads");
        }
    }
}

/// A running stand-in: the client that reaches it, and every shell service it was asked for.
struct StandIn {
    adb: AdbClient,
    commands: UnboundedReceiver<String>,
}

impl StandIn {
    /// Asserts that every shell command the smoke ran was its read-only `getprop`.
    fn assert_only_read(&mut self) {
        while let Ok(command) = self.commands.try_recv() {
            assert_eq!(
                command,
                format!("shell,v2,raw:{}", smoke::SMOKE_COMMAND),
                "the smoke runs only its read-only getprop"
            );
        }
    }
}

async fn stand_in(phones: Phones) -> StandIn {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a loopback port is free");
    let port = listener
        .local_addr()
        .expect("the listener has an address")
        .port();
    let (sender, commands) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.expect("the client connects");
            serve(&mut stream, &phones, &sender).await;
        }
    });
    StandIn {
        adb: AdbClient::new(Endpoint::new("127.0.0.1".to_owned(), port)),
        commands,
    }
}

fn two_phones() -> Phones {
    Phones {
        list: "SPARELINE00001         device usb:1-1 product:S6702X1 model:S6702X device:S6702XA \
               transport_id:1\nSPARELINE00002         unauthorized usb:1-2 transport_id:2\n",
        answers: vec![PhoneAnswer {
            serial: "SPARELINE00001",
            stdout: "S6702X\n14\n",
            exit_code: 0,
        }],
    }
}

#[test]
fn passes_and_reports_each_phone() {
    block_on(async {
        let mut stand_in_server = stand_in(two_phones()).await;
        let adb = stand_in_server.adb.clone();
        let mut out = Vec::new();
        Smoke::new(&adb, 1).run(&mut out).await.unwrap();
        let port = adb.endpoint().port();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            format!(
                "adb server 127.0.0.1:{port}: protocol 41, 2 device(s), 1 online\n  \
                 SPARELINE00002: unauthorized\n  SPARELINE00001: S6702X, Android 14\nsmoke ok\n"
            )
        );
        stand_in_server.assert_only_read();
    });
}

#[test]
fn fails_with_fewer_online_phones_than_asked() {
    block_on(async {
        let mut stand_in_server = stand_in(two_phones()).await;
        let adb = stand_in_server.adb.clone();
        let mut out = Vec::new();
        let error = Smoke::new(&adb, 2).run(&mut out).await.unwrap_err();
        assert_eq!(error.to_string(), "expected at least 2 online phone(s)");
        stand_in_server.assert_only_read();
    });
}

#[test]
fn passes_with_no_phones_when_none_are_asked_for() {
    block_on(async {
        let mut stand_in_server = stand_in(Phones {
            list: "",
            answers: Vec::new(),
        })
        .await;
        let adb = stand_in_server.adb.clone();
        let mut out = Vec::new();
        Smoke::new(&adb, 0).run(&mut out).await.unwrap();
        assert!(
            String::from_utf8(out)
                .unwrap()
                .ends_with("0 device(s), 0 online\nsmoke ok\n")
        );
        stand_in_server.assert_only_read();
    });
}

#[test]
fn a_phone_that_answers_without_a_model_fails() {
    block_on(async {
        let mut phones = two_phones();
        phones.answers[0].stdout = "\n";
        phones.answers[0].exit_code = 1;
        let mut stand_in_server = stand_in(phones).await;
        let adb = stand_in_server.adb.clone();
        let mut out = Vec::new();
        let error = Smoke::new(&adb, 0).run(&mut out).await.unwrap_err();
        assert!(matches!(error, SmokeError::PhonesSilent { count: 1 }));
        assert_eq!(error.to_string(), "1 online phone(s) did not answer");
        let report = String::from_utf8(out).unwrap();
        assert!(
            report.contains("  SPARELINE00001: FAILED exit 1, model \"\"\n"),
            "{report}"
        );
        stand_in_server.assert_only_read();
    });
}
