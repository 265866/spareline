//! A stand-in adb server: a TCP listener speaking the adb wire format, as the adb client's
//! recorded transcripts show it, in front of one [`MonitorPhone`].

use std::str;

use spareline_adb::Endpoint;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch::{self, Receiver, Sender};
use tokio::task::JoinHandle;

use crate::monitor_phone::{Answer, MonitorPhone};

/// The device list line of the phone online, as the B30's adb server printed it.
fn online_line(serial: &str) -> String {
    format!(
        "{serial}         device usb:1-1 product:S6702X1 model:S6702X device:S6702XA transport_id:1\n"
    )
}

/// A running stand-in adb server with one phone, online or not.
pub(crate) struct AdbStandIn {
    endpoint: Endpoint,
    list: Sender<String>,
    task: JoinHandle<()>,
}

impl AdbStandIn {
    /// Starts a stand-in that lists no phone yet.
    pub(crate) async fn start(phone: MonitorPhone) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a loopback port is free");
        let port = listener
            .local_addr()
            .expect("the listener has an address")
            .port();
        let (list, lists) = watch::channel(String::new());
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                stream
                    .set_nodelay(true)
                    .expect("loopback sockets take TCP_NODELAY");
                let phone = phone.clone();
                let lists = lists.clone();
                tokio::spawn(async move {
                    serve(stream, &phone, lists).await;
                });
            }
        });
        Self {
            endpoint: Endpoint::new("127.0.0.1".to_owned(), port),
            list,
            task,
        }
    }

    /// Where the stand-in listens.
    pub(crate) fn endpoint(&self) -> Endpoint {
        self.endpoint.clone()
    }

    /// Lists the phone as online, which the device trackers see at once.
    pub(crate) fn plug_in(&self, serial: &str) {
        self.list.send_replace(online_line(serial));
    }

    /// Lists no phone, as when its cable is pulled.
    pub(crate) fn unplug(&self) {
        self.list.send_replace(String::new());
    }
}

impl Drop for AdbStandIn {
    fn drop(&mut self) {
        self.task.abort();
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

async fn okay(stream: &mut TcpStream) -> Option<()> {
    stream.write_all(b"OKAY").await.ok()
}

async fn fail(stream: &mut TcpStream, message: &str) -> Option<()> {
    let framed = format!("FAIL{:04x}{message}", message.len());
    stream.write_all(framed.as_bytes()).await.ok()
}

async fn hex_string(stream: &mut TcpStream, text: &str) -> Option<()> {
    let framed = format!("{:04x}{text}", text.len());
    stream.write_all(framed.as_bytes()).await.ok()
}

fn shell_packet(id: u8, data: &[u8]) -> Vec<u8> {
    let mut packet = vec![id];
    let length = u32::try_from(data.len()).expect("a shell packet fits in 4 GiB");
    packet.extend_from_slice(&length.to_le_bytes());
    packet.extend_from_slice(data);
    packet
}

async fn serve(mut stream: TcpStream, phone: &MonitorPhone, mut lists: Receiver<String>) {
    let Some(request) = read_request(&mut stream).await else {
        return;
    };
    match request.as_str() {
        "host:version" => {
            if okay(&mut stream).await.is_some() {
                hex_string(&mut stream, "0029").await;
            }
        }
        "host:devices-l" => {
            let list = lists.borrow().clone();
            if okay(&mut stream).await.is_some() {
                hex_string(&mut stream, &list).await;
            }
        }
        "host:track-devices-l" => {
            if okay(&mut stream).await.is_none() {
                return;
            }
            loop {
                let list = lists.borrow_and_update().clone();
                if hex_string(&mut stream, &list).await.is_none() || lists.changed().await.is_err()
                {
                    return;
                }
            }
        }
        transport => {
            let Some(serial) = transport.strip_prefix("host:transport:") else {
                fail(&mut stream, "unknown host service").await;
                return;
            };
            if !lists.borrow().contains(&format!("{serial} ")) {
                fail(&mut stream, &format!("device '{serial}' not found")).await;
                return;
            }
            if okay(&mut stream).await.is_none() {
                return;
            }
            let Some(service) = read_request(&mut stream).await else {
                return;
            };
            device_service(stream, phone, serial, &service).await;
        }
    }
}

async fn device_service(mut stream: TcpStream, phone: &MonitorPhone, serial: &str, service: &str) {
    if service == "sync:" {
        if okay(&mut stream).await.is_some() {
            sync(&mut stream, phone).await;
        }
        return;
    }
    let Some(command) = service.strip_prefix("shell,v2,raw:") else {
        fail(&mut stream, &format!("unsupported service {service}")).await;
        return;
    };
    match phone.answer(serial, command).await {
        Answer::Refuse(message) => {
            fail(&mut stream, &message).await;
        }
        Answer::Output { stdout, exit_code } => {
            if okay(&mut stream).await.is_none() {
                return;
            }
            let mut packets = shell_packet(1, stdout.as_bytes());
            packets.extend(shell_packet(3, &[exit_code]));
            match stream.write_all(&packets).await {
                Ok(()) | Err(_) => {}
            }
        }
        Answer::Stream(mut lines) => {
            if okay(&mut stream).await.is_none() {
                return;
            }
            while let Some(line) = lines.recv().await {
                if stream
                    .write_all(&shell_packet(1, line.as_bytes()))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            if stream.shutdown().await.is_err() {
                return;
            }
            let mut rest = [0; 64];
            while matches!(stream.read(&mut rest).await, Ok(read) if read > 0) {}
            phone.log_stream_let_go();
        }
    }
}

/// One `sync:` record header: its four-letter id and its value or payload length.
struct SyncRecord {
    id: [u8; 4],
    value: u32,
}

async fn sync_record(stream: &mut TcpStream) -> Option<SyncRecord> {
    let mut header = [0; 8];
    stream.read_exact(&mut header).await.ok()?;
    let id = [header[0], header[1], header[2], header[3]];
    let value = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
    Some(SyncRecord { id, value })
}

async fn sync_payload(stream: &mut TcpStream, length: u32) -> Option<Vec<u8>> {
    let mut payload = vec![0; usize::try_from(length).ok()?];
    stream.read_exact(&mut payload).await.ok()?;
    Some(payload)
}

async fn sync(stream: &mut TcpStream, phone: &MonitorPhone) -> Option<()> {
    let send = sync_record(stream).await?;
    if &send.id != b"SEND" {
        return None;
    }
    let target = String::from_utf8(sync_payload(stream, send.value).await?).ok()?;
    let (path, _mode) = target.rsplit_once(',')?;
    let mut data = Vec::new();
    loop {
        let record = sync_record(stream).await?;
        match &record.id {
            b"DATA" => data.extend(sync_payload(stream, record.value).await?),
            b"DONE" => break,
            _ => return None,
        }
    }
    match phone.receive(path, &data) {
        Ok(()) => stream.write_all(b"OKAY\0\0\0\0").await.ok()?,
        Err(message) => {
            let length = u32::try_from(message.len()).ok()?;
            let mut record = b"FAIL".to_vec();
            record.extend_from_slice(&length.to_le_bytes());
            record.extend_from_slice(message.as_bytes());
            stream.write_all(&record).await.ok()?;
        }
    }
    sync_record(stream).await.map(|_quit| ())
}
