//! A stand-in adb server with the B30 plugged in, answering the phone manager's identity and
//! health probes with the B30's recorded output. A real phone can't be plugged in on demand, so
//! this speaks the adb wire format the adb client's recorded transcripts show; everything behind
//! it, from the phone manager to the routes, is real.

use std::str;

use spareline_adb::Endpoint;
use tokio::io::AsyncReadExt as _;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

/// The B30's serial in the recordings.
pub(crate) const B30: &str = "SPARELINE00001";

const IDENTITY: &str = include_str!("../fixtures/nuu-s6702x/identity.txt");
const SUBSCRIBER: &str = include_str!("../fixtures/nuu-s6702x/subscriber.txt");
const HEALTH: &str = include_str!("../fixtures/nuu-s6702x/health.txt");

/// The B30's line in the device list, as its adb server printed it.
const DEVICE_LIST: &str = "SPARELINE00001         device usb:1-1 product:S6702X1 model:S6702X device:S6702XA transport_id:1\n";

/// A running stand-in, stopped when dropped.
pub(crate) struct B30StandIn {
    endpoint: Endpoint,
    task: JoinHandle<()>,
}

impl Drop for B30StandIn {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl B30StandIn {
    /// Starts the stand-in on a loopback port.
    pub(crate) async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a loopback port is free");
        let port = listener.local_addr().expect("a bound address").port();
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(serve(stream));
            }
        });
        Self {
            endpoint: Endpoint::new("127.0.0.1".to_owned(), port),
            task,
        }
    }

    /// Where it listens.
    pub(crate) fn endpoint(&self) -> Endpoint {
        self.endpoint.clone()
    }
}

async fn read_request(stream: &mut TcpStream) -> Option<String> {
    let mut length = [0; 4];
    stream.read_exact(&mut length).await.ok()?;
    let length = usize::from_str_radix(str::from_utf8(&length).ok()?, 16).ok()?;
    let mut body = vec![0; length];
    stream.read_exact(&mut body).await.ok()?;
    String::from_utf8(body).ok()
}

async fn write(stream: &mut TcpStream, bytes: &[u8]) -> Option<()> {
    use tokio::io::AsyncWriteExt as _;

    stream.write_all(bytes).await.ok()
}

async fn hex_string(stream: &mut TcpStream, text: &str) -> Option<()> {
    write(stream, format!("{:04x}{text}", text.len()).as_bytes()).await
}

fn shell_packet(id: u8, data: &[u8]) -> Vec<u8> {
    let mut packet = vec![id];
    let length = u32::try_from(data.len()).expect("a shell packet fits in 4 GiB");
    packet.extend_from_slice(&length.to_le_bytes());
    packet.extend_from_slice(data);
    packet
}

/// What a shell call prints, and its exit status.
struct Output {
    stdout: &'static str,
    exit_code: u8,
}

impl Output {
    fn ok(stdout: &'static str) -> Self {
        Self {
            stdout,
            exit_code: 0,
        }
    }
}

/// What the B30 prints for `command`, and its exit status.
fn answer(command: &str) -> Output {
    if command == spareline_core::identity_command() {
        Output::ok(IDENTITY)
    } else if command.starts_with("printf '\\n@@sl:imei:0\\n'") {
        Output::ok(SUBSCRIBER)
    } else if command == spareline_core::health_command() {
        Output::ok(HEALTH)
    } else {
        Output {
            stdout: "",
            exit_code: 127,
        }
    }
}

async fn serve(mut stream: TcpStream) -> Option<()> {
    let request = read_request(&mut stream).await?;
    match request.as_str() {
        "host:version" => {
            write(&mut stream, b"OKAY").await?;
            hex_string(&mut stream, "0029").await
        }
        "host:devices-l" => {
            write(&mut stream, b"OKAY").await?;
            hex_string(&mut stream, DEVICE_LIST).await
        }
        "host:track-devices-l" => {
            write(&mut stream, b"OKAY").await?;
            hex_string(&mut stream, DEVICE_LIST).await?;
            let mut rest = Vec::new();
            stream.read_to_end(&mut rest).await.ok().map(|_| ())
        }
        transport if transport == format!("host:transport:{B30}") => {
            write(&mut stream, b"OKAY").await?;
            let service = read_request(&mut stream).await?;
            let command = service.strip_prefix("shell,v2,raw:")?;
            let Output { stdout, exit_code } = answer(command);
            write(&mut stream, b"OKAY").await?;
            let mut packets = shell_packet(1, stdout.as_bytes());
            packets.extend(shell_packet(3, &[exit_code]));
            write(&mut stream, &packets).await
        }
        _ => {
            let message = "unknown service";
            write(
                &mut stream,
                format!("FAIL{:04x}{message}", message.len()).as_bytes(),
            )
            .await
        }
    }
}
