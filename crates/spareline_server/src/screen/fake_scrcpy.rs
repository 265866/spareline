//! A stand-in for the phone side of a screen session, behind a stand-in adb server speaking the
//! real wire format. A real session needs a phone running scrcpy-server, and these tests need
//! to end streams, crash servers, hold a start halfway, leave orphan servers and unplug phones
//! on demand. The video bytes it serves are the real recording from the B30
//! (`tests/fixtures/screen/grid-home.bin`).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use std::{fs, pin, str};

use futures_util::future::{self as futures, Either};
use spareline_adb::{AdbClient, Endpoint};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::sync::watch::{self, Receiver, Sender};
use tokio::time::{self, Instant};

/// The recording's dummy byte and video header: 1 + 64 + 12 bytes.
const HEADER_BYTES: usize = 77;

/// The recorded video socket: the dummy byte, the header, then 24 packets.
pub(crate) fn recorded() -> Vec<u8> {
    fs::read(fixture_path("screen/grid-home.bin")).expect("the recording is readable")
}

/// The recording's packets, header and payload each.
pub(crate) fn recorded_packets() -> Vec<Vec<u8>> {
    let bytes = recorded();
    let mut packets = Vec::new();
    let mut offset = HEADER_BYTES;
    while offset < bytes.len() {
        let size = u32::from_be_bytes(bytes[offset + 8..offset + 12].try_into().unwrap());
        let end = offset + 12 + usize::try_from(size).unwrap();
        packets.push(bytes[offset..end].to_vec());
        offset = end;
    }
    packets
}

/// Whether a packet's header marks a config packet.
pub(crate) fn is_config(packet: &[u8]) -> bool {
    packet.first().is_some_and(|flags| flags & 0x80 != 0)
}

/// Whether a packet's header marks a keyframe.
pub(crate) fn is_keyframe(packet: &[u8]) -> bool {
    packet.first().is_some_and(|flags| flags & 0x40 != 0)
}

fn fixture_path(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(path)
}

/// What the server sends after `RESET_VIDEO`: the recording's first config packet and the
/// keyframe after it.
fn reset_packets() -> [Vec<u8>; 2] {
    let packets = recorded_packets();
    let config = packets
        .iter()
        .position(|packet| is_config(packet))
        .expect("the recording has a config packet");
    [packets[config].clone(), packets[config + 1].clone()]
}

/// A scrcpy process left on the phone from before, a crashed relay or someone's own scrcpy.
/// The pkill of any server ends it unless it is `unkillable`; its `CleanUp` then runs for
/// `cleanup`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Orphan {
    pub(crate) unkillable: bool,
    pub(crate) cleanup: Duration,
}

/// What a test sees of one server the relay started.
#[derive(Clone, Debug)]
pub(crate) struct ServerView {
    pub(crate) command: String,
    pub(crate) alive: bool,
    pub(crate) control: Vec<Vec<u8>>,
}

struct Server {
    command: String,
    alive: bool,
    control: Vec<Vec<u8>>,
    video: Option<UnboundedSender<Vec<u8>>>,
    dead: Sender<bool>,
}

impl Server {
    fn crash(&mut self) {
        self.alive = false;
        self.video = None;
        self.dead.send_replace(true);
    }
}

/// What the wake command does.
#[derive(Clone, Copy)]
struct WakeBehavior {
    sticks: bool,
    fails: bool,
    delay: Duration,
    done: usize,
}

struct State {
    online: bool,
    servers: Vec<Server>,
    pushes: usize,
    kills: Vec<String>,
    kills_done: Vec<String>,
    kill_delay: Duration,
    orphan: Option<Orphan>,
    orphan_gone_at: Option<Instant>,
    log: Vec<String>,
    wakefulness: &'static str,
    wake: WakeBehavior,
    wakefulness_reads: usize,
    answer_resets: bool,
    silent_video: bool,
}

struct Shared {
    state: Mutex<State>,
    gate: Receiver<bool>,
    /// Whether servers stop reading their control socket.
    control_stalled: AtomicBool,
}

/// A running stand-in phone and adb server. Its tasks end with the test's runtime. Its sockets
/// send each write at once: under Nagle's algorithm a packet written right after another waits
/// for an acknowledgement in real time, which tokio's paused clock outruns.
pub(crate) struct FakePhone {
    port: u16,
    #[cfg_attr(
        dylint_lib = "trellis_lints",
        expect(
            shared_ownership,
            reason = "the test and every connection task the stand-in spawns read and write the \
                      same phone"
        )
    )]
    shared: Arc<Shared>,
    gate: Sender<bool>,
}

impl FakePhone {
    /// An online phone with no scrcpy process, awake.
    pub(crate) async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a loopback port is free");
        let port = listener
            .local_addr()
            .expect("the listener has an address")
            .port();
        let (gate, gate_receiver) = watch::channel(false);
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                online: true,
                servers: Vec::new(),
                pushes: 0,
                kills: Vec::new(),
                kills_done: Vec::new(),
                kill_delay: Duration::ZERO,
                orphan: None,
                orphan_gone_at: None,
                log: Vec::new(),
                wakefulness: "Awake",
                wake: WakeBehavior {
                    sticks: true,
                    fails: false,
                    delay: Duration::ZERO,
                    done: 0,
                },
                wakefulness_reads: 0,
                answer_resets: true,
                silent_video: false,
            }),
            gate: gate_receiver,
            control_stalled: AtomicBool::new(false),
        });
        let accepting = Arc::clone(&shared);
        drop(tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                stream
                    .set_nodelay(true)
                    .expect("loopback sockets take TCP_NODELAY");
                drop(tokio::spawn(serve(stream, Arc::clone(&accepting))));
            }
        }));
        Self { port, shared, gate }
    }

    /// A client of the stand-in adb server.
    pub(crate) fn client(&self) -> AdbClient {
        AdbClient::new(Endpoint::new("127.0.0.1".to_owned(), self.port))
    }

    /// Every server the relay started, in order.
    pub(crate) fn servers(&self) -> Vec<ServerView> {
        self.state()
            .servers
            .iter()
            .map(|server| ServerView {
                command: server.command.clone(),
                alive: server.alive,
                control: server.control.clone(),
            })
            .collect()
    }

    /// How many times the jar was pushed.
    pub(crate) fn pushes(&self) -> usize {
        self.state().pushes
    }

    /// Every pkill command run, as it started.
    pub(crate) fn kills(&self) -> Vec<String> {
        self.state().kills.clone()
    }

    /// Every pkill command that finished.
    pub(crate) fn kills_done(&self) -> Vec<String> {
        self.state().kills_done.clone()
    }

    /// The wake commands and control messages, in order: `exec:<command>` and
    /// `control:<type>`.
    pub(crate) fn log(&self) -> Vec<String> {
        self.state().log.clone()
    }

    /// How many times `dumpsys power` was read.
    pub(crate) fn wakefulness_reads(&self) -> usize {
        self.state().wakefulness_reads
    }

    /// What `dumpsys power` reports.
    pub(crate) fn wakefulness(&self) -> &'static str {
        self.state().wakefulness
    }

    /// Sets what `dumpsys power` reports: `Awake` or `Dozing`.
    pub(crate) fn set_wakefulness(&self, state: &'static str) {
        self.state().wakefulness = state;
    }

    /// Whether the wake command makes the phone awake.
    pub(crate) fn set_wake_sticks(&self, sticks: bool) {
        self.state().wake.sticks = sticks;
    }

    /// Whether the wake command fails.
    pub(crate) fn set_wake_fails(&self, fails: bool) {
        self.state().wake.fails = fails;
    }

    /// How long each pkill takes.
    pub(crate) fn set_kill_delay(&self, delay: Duration) {
        self.state().kill_delay = delay;
    }

    /// Leaves an orphan scrcpy process on the phone.
    pub(crate) fn set_orphan(&self, orphan: Orphan) {
        self.state().orphan = Some(orphan);
    }

    /// Whether the server answers `RESET_VIDEO` at once.
    pub(crate) fn set_answer_resets(&self, answer: bool) {
        self.state().answer_resets = answer;
    }

    /// Holds every video socket before it opens, holding starts halfway, until
    /// [`FakePhone::release_starts`].
    pub(crate) fn hold_starts(&self) {
        self.gate.send_replace(true);
    }

    /// Lets held video sockets open.
    pub(crate) fn release_starts(&self) {
        self.gate.send_replace(false);
    }

    /// Server `index` sends a config packet and a keyframe, as after `RESET_VIDEO`.
    pub(crate) fn answer_reset(&self, index: usize) {
        let state = self.state();
        if let Some(video) = state
            .servers
            .get(index)
            .and_then(|server| server.video.as_ref())
        {
            for packet in reset_packets() {
                match video.send(packet) {
                    Ok(()) | Err(_) => {}
                }
            }
        }
    }

    /// How long the wake command takes.
    pub(crate) fn set_wake_delay(&self, delay: Duration) {
        self.state().wake.delay = delay;
    }

    /// How many wake commands have finished.
    pub(crate) fn wakes_done(&self) -> usize {
        self.state().wake.done
    }

    /// Whether video sockets open without the server's dummy byte, as before it listens.
    pub(crate) fn set_silent_video(&self, silent: bool) {
        self.state().silent_video = silent;
    }

    /// Every server stops reading its control socket after the read in flight, as a stalled
    /// server does, so the relay's writes back up.
    pub(crate) fn stall_control(&self) {
        self.shared.control_stalled.store(true, Ordering::Release);
    }

    /// Server `index` sends a packet with the header flags `flags` (`0x80` config, `0x40`
    /// keyframe, `0` a frame) and a payload of `size` zero bytes.
    pub(crate) fn send_packet(&self, index: usize, flags: u8, size: usize) {
        let mut packet = vec![0; 12 + size];
        packet[0] = flags;
        packet[8..12].copy_from_slice(&u32::try_from(size).unwrap().to_be_bytes());
        let state = self.state();
        if let Some(video) = state
            .servers
            .get(index)
            .and_then(|server| server.video.as_ref())
        {
            match video.send(packet) {
                Ok(()) | Err(_) => {}
            }
        }
    }

    /// Server `index` sends a keyframe on its own, with no config packet before it.
    pub(crate) fn send_keyframe(&self, index: usize) {
        let state = self.state();
        if let Some(video) = state
            .servers
            .get(index)
            .and_then(|server| server.video.as_ref())
        {
            let [_config, keyframe] = reset_packets();
            match video.send(keyframe) {
                Ok(()) | Err(_) => {}
            }
        }
    }

    /// Server `index` crashes: its process exits and its sockets close.
    pub(crate) fn crash(&self, index: usize) {
        if let Some(server) = self.state().servers.get_mut(index) {
            server.crash();
        }
    }

    /// Every server crashes and adb calls stop reaching the phone, as when it is unplugged.
    pub(crate) fn unplug(&self) {
        let mut state = self.state();
        state.online = false;
        for server in &mut state.servers {
            server.crash();
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.shared.state.lock().unwrap()
    }
}

#[cfg_attr(
    dylint_lib = "trellis_lints",
    expect(
        shared_ownership,
        reason = "each connection runs in a task of its own, which outlives the accepting loop's \
                  borrow"
    )
)]
async fn serve(mut stream: TcpStream, shared: Arc<Shared>) {
    let Some(request) = read_request(&mut stream).await else {
        return;
    };
    if request == "host:version" {
        write(&mut stream, b"OKAY00040029").await;
    } else if request.starts_with("host:transport:") {
        write(&mut stream, b"OKAY").await;
        let Some(service) = read_request(&mut stream).await else {
            return;
        };
        if !shared.state.lock().unwrap().online {
            fail(&mut stream, "device offline (stand-in)").await;
        } else if let Some(command) = service.strip_prefix("exec:") {
            exec(stream, &shared, command).await;
        } else if service == "sync:" {
            write(&mut stream, b"OKAY").await;
            if receive_push(&mut stream).await {
                shared.state.lock().unwrap().pushes += 1;
            }
        } else if service.starts_with("localabstract:scrcpy_") {
            socket(stream, &shared).await;
        } else {
            fail(&mut stream, &format!("{service}: not recorded")).await;
        }
    } else {
        fail(&mut stream, &format!("unknown request {request}")).await;
    }
}

async fn exec(mut stream: TcpStream, shared: &Shared, command: &str) {
    if command.starts_with("pgrep") {
        let running = {
            let state = shared.state.lock().unwrap();
            state.orphan.is_some() || state.orphan_gone_at.is_some_and(|at| Instant::now() < at)
        };
        let output: &[u8] = if running { b"4242\n" } else { b"" };
        reply(&mut stream, output).await;
    } else if command.starts_with("input ") {
        let wake = {
            let mut state = shared.state.lock().unwrap();
            state.log.push(format!("exec:{command}"));
            if !state.wake.fails && state.wake.sticks {
                state.wakefulness = "Awake";
            }
            state.wake
        };
        if !wake.delay.is_zero() {
            time::sleep(wake.delay).await;
        }
        shared.state.lock().unwrap().wake.done += 1;
        if wake.fails {
            fail(&mut stream, "input: device busy (stand-in)").await;
        } else {
            reply(&mut stream, b"").await;
        }
    } else if command.starts_with("dumpsys power") {
        let state = {
            let mut state = shared.state.lock().unwrap();
            state.wakefulness_reads += 1;
            state.wakefulness
        };
        let name = format!("device/nuu-s6702x/wakefulness-{}.txt", state.to_lowercase());
        let output = fs::read(fixture_path(&name)).expect("the wakefulness fixture is readable");
        reply(&mut stream, &output).await;
    } else if command.starts_with("pkill") {
        let delay = {
            let mut state = shared.state.lock().unwrap();
            state.kills.push(command.to_owned());
            state.kill_delay
        };
        if !delay.is_zero() {
            time::sleep(delay).await;
        }
        {
            let mut state = shared.state.lock().unwrap();
            state.kills_done.push(command.to_owned());
            if command.contains("scrcpy.Serve[r]")
                && let Some(orphan) = state.orphan
                && !orphan.unkillable
            {
                state.orphan_gone_at = Some(Instant::now() + orphan.cleanup);
                state.orphan = None;
            }
        }
        reply(&mut stream, b"").await;
    } else if command.starts_with("CLASSPATH=") {
        run_server(stream, shared, command).await;
    } else {
        reply(&mut stream, b"").await;
    }
}

/// A server runs as long as its `exec:` stream: until it crashes, or the client closes the
/// stream, which has adbd kill it.
async fn run_server(mut stream: TcpStream, shared: &Shared, command: &str) {
    let (dead, mut died) = watch::channel(false);
    let index = {
        let mut state = shared.state.lock().unwrap();
        state.servers.push(Server {
            command: command.to_owned(),
            alive: true,
            control: Vec::new(),
            video: None,
            dead,
        });
        state.servers.len() - 1
    };
    write(&mut stream, b"OKAY").await;
    let (mut reader, writer) = stream.into_split();
    let client_closed = pin::pin!(closed(&mut reader));
    let server_died = pin::pin!(died_off(&mut died));
    if let Either::Left(((), _)) = futures::select(client_closed, server_died).await
        && let Some(server) = shared.state.lock().unwrap().servers.get_mut(index)
    {
        server.crash();
    }
    drop(writer);
}

/// The first connection to a live server is its video socket: the recording, then whatever the
/// test sends, until the server dies. The second is its control socket, which answers
/// `RESET_VIDEO` with a config packet and a keyframe.
async fn socket(mut stream: TcpStream, shared: &Shared) {
    let opened = {
        let state = shared.state.lock().unwrap();
        state
            .servers
            .len()
            .checked_sub(1)
            .filter(|&last| state.servers[last].alive)
            .map(|last| {
                (
                    last,
                    state.servers[last].video.is_some(),
                    state.servers[last].dead.subscribe(),
                )
            })
    };
    let Some((index, has_video, mut died)) = opened else {
        fail(&mut stream, "localabstract: closed (stand-in)").await;
        return;
    };
    if has_video {
        control_socket(stream, shared, index, died).await;
        return;
    }
    let mut gate = shared.gate.clone();
    if gate.wait_for(|held| !*held).await.is_err() {
        return;
    }
    let (sender, mut packets) = mpsc::unbounded_channel();
    let alive = {
        let mut state = shared.state.lock().unwrap();
        let server = &mut state.servers[index];
        if server.alive {
            server.video = Some(sender);
        }
        server.alive
    };
    if !alive {
        fail(&mut stream, "localabstract: closed (stand-in)").await;
        return;
    }
    write(&mut stream, b"OKAY").await;
    let silent = shared.state.lock().unwrap().silent_video;
    if !silent {
        write(&mut stream, &recorded()).await;
    }
    let (mut reader, mut writer) = stream.into_split();
    loop {
        let packet = pin::pin!(packets.recv());
        let server_died = pin::pin!(died_off(&mut died));
        let client_closed = pin::pin!(closed(&mut reader));
        let ended = futures::select(server_died, client_closed);
        let Either::Left((Some(packet), _)) = futures::select(packet, ended).await else {
            return;
        };
        if writer.write_all(&packet).await.is_err() {
            return;
        }
    }
}

async fn control_socket(
    stream: TcpStream,
    shared: &Shared,
    index: usize,
    mut died: Receiver<bool>,
) {
    let (mut reader, writer) = stream.into_split();
    let mut writer: OwnedWriteHalf = writer;
    if writer.write_all(b"OKAY").await.is_err() {
        return;
    }
    let mut pending = Vec::new();
    loop {
        if shared.control_stalled.load(Ordering::Acquire) {
            died_off(&mut died).await;
            return;
        }
        let read = pin::pin!(read_some(&mut reader));
        let server_died = pin::pin!(died_off(&mut died));
        let Either::Left((Some(bytes), _)) = futures::select(read, server_died).await else {
            return;
        };
        pending.extend(bytes);
        while let Some(length) = message_length(&pending) {
            let message: Vec<u8> = pending.drain(..length).collect();
            let answer = {
                let mut state = shared.state.lock().unwrap();
                state.log.push(format!("control:{}", message[0]));
                state.servers[index].control.push(message.clone());
                message == [17] && state.answer_resets
            };
            if answer {
                let state = shared.state.lock().unwrap();
                if let Some(video) = state.servers[index].video.as_ref() {
                    for packet in reset_packets() {
                        match video.send(packet) {
                            Ok(()) | Err(_) => {}
                        }
                    }
                }
            }
        }
    }
}

/// The length of the first whole control message in `bytes`, by scrcpy's message types.
fn message_length(bytes: &[u8]) -> Option<usize> {
    let length = match *bytes.first()? {
        0 => 14,
        1 => 5 + usize::try_from(u32::from_be_bytes(bytes.get(1..5)?.try_into().ok()?)).ok()?,
        2 => 32,
        3 => 21,
        4 | 10 => 2,
        17 => 1,
        _ => bytes.len(),
    };
    (bytes.len() >= length).then_some(length)
}

async fn read_some(reader: &mut OwnedReadHalf) -> Option<Vec<u8>> {
    let mut buffer = vec![0; 4096];
    match reader.read(&mut buffer).await {
        Ok(0) | Err(_) => None,
        Ok(read) => Some(buffer[..read].to_vec()),
    }
}

async fn receive_push(stream: &mut TcpStream) -> bool {
    loop {
        let mut header = [0; 8];
        if stream.read_exact(&mut header).await.is_err() {
            return false;
        }
        let length = usize::try_from(u32::from_le_bytes(header[4..].try_into().unwrap())).unwrap();
        match &header[..4] {
            b"SEND" | b"DATA" => {
                let mut body = vec![0; length];
                if stream.read_exact(&mut body).await.is_err() {
                    return false;
                }
            }
            b"DONE" => {
                write(stream, b"OKAY\0\0\0\0").await;
                return true;
            }
            _ => return false,
        }
    }
}

/// Answers an `exec:` with `output`, then closes.
async fn reply(stream: &mut TcpStream, output: &[u8]) {
    let mut bytes = b"OKAY".to_vec();
    bytes.extend_from_slice(output);
    write(stream, &bytes).await;
}

async fn read_request(stream: &mut TcpStream) -> Option<String> {
    let mut length = [0; 4];
    stream.read_exact(&mut length).await.ok()?;
    let length = usize::from_str_radix(str::from_utf8(&length).ok()?, 16).ok()?;
    let mut body = vec![0; length];
    stream.read_exact(&mut body).await.ok()?;
    String::from_utf8(body).ok()
}

/// Finishes when the server has died.
async fn died_off(died: &mut Receiver<bool>) {
    match died.wait_for(|dead| *dead).await {
        Ok(_) | Err(_) => {}
    }
}

/// Finishes when the client closes its side or the connection fails.
async fn closed(reader: &mut OwnedReadHalf) {
    let mut byte = [0; 1];
    while matches!(reader.read(&mut byte).await, Ok(read) if read > 0) {}
}

async fn fail(stream: &mut TcpStream, message: &str) {
    write(
        stream,
        format!("FAIL{:04x}{message}", message.len()).as_bytes(),
    )
    .await;
}

/// Writes `bytes`, ignoring a client that went away, which tests of cancellation cause.
async fn write(stream: &mut TcpStream, bytes: &[u8]) {
    match stream.write_all(bytes).await {
        Ok(()) | Err(_) => {}
    }
}
