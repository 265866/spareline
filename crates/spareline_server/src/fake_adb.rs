//! A stand-in adb server for tests: a TCP listener speaking the adb wire format. A phone can't
//! be plugged, unplugged or given a new text on demand, so tests feed the device lists the adb
//! server really sent during a transport reconnect, and answer shell commands with output
//! recorded from the B30 (`tests/fixtures/`), or with stand-ins the test writes.

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use std::{fs, io, pin, str};

use futures_util::future::{self, Either};
use serde_json::Value;
use spareline_adb::{AdbClient, Endpoint};
use spareline_core::Timestamp;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::tcp::ReadHalf;
use tokio::net::{TcpListener, TcpStream};
use tokio::runtime::Builder;
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::sync::watch::{self, Receiver, Sender};
use tokio::time;

/// The B30's serial in every recording.
pub(crate) const B30: &str = "SPARELINE00001";

/// 2026-10-22T08:00:00.000Z, the time spareline-old's ingest tests start at.
pub(crate) const START: Timestamp = Timestamp::from_millis(1_792_656_000_000);

/// Runs `future` on a current-thread runtime whose clock is paused, so timers fire as soon as
/// the runtime has nothing else to do and tests never wait for real time.
pub(crate) fn block_on<F: Future>(future: F) -> F::Output {
    Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .expect("a current-thread runtime builds")
        .block_on(future)
}

/// Waits until `check` holds, letting the paused clock run in 5 ms steps, at most 60 s of it.
pub(crate) async fn until(what: &str, mut check: impl FnMut() -> bool) {
    for _ in 0..12_000 {
        if check() {
            return;
        }
        time::sleep(Duration::from_millis(5)).await;
    }
    panic!("timed out waiting for {what}");
}

/// The text of a recorded fixture under `tests/fixtures/`.
pub(crate) fn fixture(path: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(path);
    fs::read_to_string(&path).expect("the fixture is readable")
}

/// A B30 output fixture, from `tests/fixtures/device/nuu-s6702x/`.
pub(crate) fn b30_fixture(name: &str) -> String {
    fixture(&format!("device/nuu-s6702x/{name}"))
}

/// The device lists the adb server sent while the B30's transport was kicked
/// (`tests/fixtures/adb/track-devices-reconnect.json`), as the raw payloads, in order.
pub(crate) fn recorded_track_lists() -> Vec<String> {
    let json: Value = serde_json::from_str(&fixture("adb/track-devices-reconnect.json"))
        .expect("the transcript is JSON");
    let mut stream = Vec::new();
    for event in json["connections"][0]
        .as_array()
        .expect("the transcript has a connection")
    {
        if event["from"] == "server" {
            let hex = event["hex"].as_str().expect("an event has hex bytes");
            let bytes: Vec<u8> = hex
                .as_bytes()
                .chunks(2)
                .map(|pair| {
                    u8::from_str_radix(str::from_utf8(pair).unwrap(), 16)
                        .expect("transcript bytes are hex")
                })
                .collect();
            stream.extend(bytes);
        }
    }
    let mut rest = stream.strip_prefix(b"OKAY").expect("the server said OKAY");
    let mut lists = Vec::new();
    while rest.len() >= 4 {
        let length = usize::from_str_radix(str::from_utf8(&rest[..4]).unwrap(), 16).unwrap();
        lists.push(String::from_utf8(rest[4..4 + length].to_vec()).unwrap());
        rest = &rest[4 + length..];
    }
    lists
}

/// What a shell command prints and returns.
#[derive(Clone, Debug, Default)]
pub(crate) struct Answer {
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
    pub(crate) exit_code: u8,
}

impl Answer {
    /// Exit 0 with `stdout` and nothing on stderr.
    pub(crate) fn stdout(stdout: &str) -> Self {
        Self {
            stdout: stdout.as_bytes().to_vec(),
            ..Self::default()
        }
    }
}

/// What `content read` of an MMS part gives.
#[derive(Clone, Debug)]
pub(crate) enum PartRead {
    /// Output and an exit code.
    Output(Answer),
    /// Some stdout, then the connection closes without an exit packet.
    NoExitPacket,
    /// Some stdout, then an exit packet with no code in it, which breaks the shell protocol.
    ExitWithoutCode,
    /// Nothing, until the client gives up and closes the connection.
    Hang,
    /// The adb server refuses the call with this message, as it does for a phone that went
    /// offline.
    Refused(&'static str),
}

/// A file pushed with `sync:`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Push {
    pub(crate) serial: String,
    pub(crate) remote: String,
    pub(crate) mode: u32,
    pub(crate) size: usize,
}

/// The answer to every command that starts with `prefix`.
struct PrefixAnswer {
    prefix: String,
    answer: Answer,
}

enum TrackUpdate {
    List(String),
    End,
}

#[derive(Default)]
struct State {
    reachable: bool,
    version_calls: usize,
    calls: Vec<String>,
    exec_calls: Vec<String>,
    answers: HashMap<String, Answer>,
    prefixes: Vec<PrefixAnswer>,
    streams: HashMap<String, Vec<String>>,
    parts: HashMap<i64, PartRead>,
    refusals: HashMap<String, String>,
    failing: Option<String>,
    pushes: Vec<Push>,
}

/// What every connection's task shares.
struct Shared {
    state: Mutex<State>,
    track: AsyncMutex<UnboundedReceiver<TrackUpdate>>,
    hold: Receiver<bool>,
}

/// A running stand-in adb server. Its tasks end with the test's runtime.
pub(crate) struct FakeAdb {
    port: u16,
    #[cfg_attr(
        dylint_lib = "trellis_lints",
        expect(
            shared_ownership,
            reason = "the test and every connection task the stand-in spawns read and write the \
                      same recorded calls and answers"
        )
    )]
    shared: Arc<Shared>,
    track: UnboundedSender<TrackUpdate>,
    hold: Sender<bool>,
}

impl FakeAdb {
    /// A stand-in that answers nothing yet, and is reachable.
    pub(crate) async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a loopback port is free");
        let port = listener
            .local_addr()
            .expect("the listener has an address")
            .port();
        let (track, updates) = mpsc::unbounded_channel();
        let (hold, hold_receiver) = watch::channel(false);
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                reachable: true,
                ..State::default()
            }),
            track: AsyncMutex::new(updates),
            hold: hold_receiver,
        });
        let accepting = Arc::clone(&shared);
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                stream
                    .set_nodelay(true)
                    .expect("a loopback socket takes TCP_NODELAY");
                tokio::spawn(serve(stream, Arc::clone(&accepting)));
            }
        });
        Self {
            port,
            shared,
            track,
            hold,
        }
    }

    /// A stand-in that answers the phone manager's probes with the B30's recorded output.
    pub(crate) async fn b30() -> Self {
        let adb = Self::start().await;
        adb.answer(
            &spareline_core::identity_command(),
            &b30_fixture("identity.txt"),
        );
        adb.answer_prefix("printf '\\n@@sl:imei:0\\n'", &b30_fixture("subscriber.txt"));
        adb.answer(
            &spareline_core::health_command(),
            &b30_fixture("health.txt"),
        );
        adb
    }

    /// A client of this stand-in.
    pub(crate) fn client(&self) -> AdbClient {
        AdbClient::new(Endpoint::new("127.0.0.1".to_owned(), self.port))
    }

    /// Answers `command` with `stdout` and exit 0.
    pub(crate) fn answer(&self, command: &str, stdout: &str) {
        self.answer_output(command, Answer::stdout(stdout));
    }

    /// Answers `command` with `answer`.
    pub(crate) fn answer_output(&self, command: &str, answer: Answer) {
        let mut state = self.state();
        state.refusals.remove(command);
        state.answers.insert(command.to_owned(), answer);
    }

    /// Answers every command that starts with `prefix`, and has no answer of its own, with
    /// `stdout`.
    pub(crate) fn answer_prefix(&self, prefix: &str, stdout: &str) {
        self.state().prefixes.push(PrefixAnswer {
            prefix: prefix.to_owned(),
            answer: Answer::stdout(stdout),
        });
    }

    /// Answers `command` by streaming `chunks` as stdout packets, then keeping the command
    /// running until the client closes.
    pub(crate) fn answer_stream(&self, command: &str, chunks: &[&str]) {
        let chunks: Vec<String> = chunks.iter().map(|chunk| (*chunk).to_owned()).collect();
        self.state().streams.insert(command.to_owned(), chunks);
    }

    /// Answers `content read` of the MMS part `id` with `read`. A part without one reads as
    /// empty.
    pub(crate) fn set_part(&self, id: i64, read: PartRead) {
        self.state().parts.insert(id, read);
    }

    /// Refuses `command` with `message`, as the adb server refuses a call to a phone that went
    /// away, until [`FakeAdb::answer`] answers it again.
    pub(crate) fn refuse(&self, command: &str, message: &str) {
        self.state()
            .refusals
            .insert(command.to_owned(), message.to_owned());
    }

    /// While set, every shell command fails with `message`.
    pub(crate) fn set_failing(&self, message: Option<&str>) {
        self.state().failing = message.map(str::to_owned);
    }

    /// Whether `host:version` answers.
    pub(crate) fn set_reachable(&self, reachable: bool) {
        self.state().reachable = reachable;
    }

    /// How many times `host:version` was asked for.
    pub(crate) fn version_calls(&self) -> usize {
        self.state().version_calls
    }

    /// Every shell command run, as `<serial>: <command>`.
    pub(crate) fn calls(&self) -> Vec<String> {
        self.state().calls.clone()
    }

    /// Every `exec:` command run, as `<serial>: <command>`.
    pub(crate) fn exec_calls(&self) -> Vec<String> {
        self.state().exec_calls.clone()
    }

    /// Every file pushed.
    pub(crate) fn pushes(&self) -> Vec<Push> {
        self.state().pushes.clone()
    }

    /// Holds every shell command after it is recorded, like a phone that is slow to answer,
    /// until [`FakeAdb::release`].
    pub(crate) fn hold(&self) {
        self.hold.send_replace(true);
    }

    /// Lets held shell commands answer.
    pub(crate) fn release(&self) {
        self.hold.send_replace(false);
    }

    /// Sends the device list `payload` to the client following the list.
    pub(crate) fn send_list(&self, payload: &str) {
        self.track
            .send(TrackUpdate::List(payload.to_owned()))
            .expect("the stand-in runs");
    }

    /// Closes the device-list stream, as an adb server that stops does.
    pub(crate) fn end_track(&self) {
        self.track
            .send(TrackUpdate::End)
            .expect("the stand-in runs");
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
        let reachable = {
            let mut state = shared.state.lock().unwrap();
            state.version_calls += 1;
            state.reachable
        };
        if reachable {
            write(&mut stream, b"OKAY00040029").await;
        } else {
            fail(&mut stream, "cannot reach the adb server (stand-in)").await;
        }
    } else if request == "host:track-devices-l" {
        track(stream, &shared).await;
    } else if let Some(serial) = request.strip_prefix("host:transport:") {
        let serial = serial.to_owned();
        write(&mut stream, b"OKAY").await;
        if let Some(service) = read_request(&mut stream).await {
            device_service(stream, &shared, &serial, &service).await;
        }
    } else {
        fail(&mut stream, &format!("unknown request {request}")).await;
    }
}

async fn track(mut stream: TcpStream, shared: &Shared) {
    let mut updates = shared.track.lock().await;
    let (mut reader, mut writer) = stream.split();
    if writer.write_all(b"OKAY").await.is_err() {
        return;
    }
    loop {
        let update = pin::pin!(updates.recv());
        let gone = pin::pin!(closed(&mut reader));
        match future::select(update, gone).await {
            Either::Left((Some(TrackUpdate::List(payload)), _)) => {
                let framed = format!("{:04x}{payload}", payload.len());
                if writer.write_all(framed.as_bytes()).await.is_err() {
                    return;
                }
            }
            Either::Left((Some(TrackUpdate::End) | None, _)) | Either::Right(((), _)) => return,
        }
    }
}

async fn device_service(mut stream: TcpStream, shared: &Shared, serial: &str, service: &str) {
    if let Some(command) = service.strip_prefix("shell,v2,raw:") {
        shell(stream, shared, serial, command).await;
    } else if let Some(command) = service.strip_prefix("exec:") {
        shared
            .state
            .lock()
            .unwrap()
            .exec_calls
            .push(format!("{serial}: {command}"));
        write(&mut stream, b"OKAYexec-ok").await;
    } else if service == "sync:" {
        write(&mut stream, b"OKAY").await;
        if let Some(push) = receive_push(&mut stream, serial).await {
            shared.state.lock().unwrap().pushes.push(push);
        }
    } else if service == "localabstract:echo" {
        write(&mut stream, b"OKAY").await;
        let (mut reader, mut writer) = stream.split();
        let mut buffer = [0; 4096];
        while let Ok(read) = reader.read(&mut buffer).await
            && read > 0
        {
            if writer.write_all(&buffer[..read]).await.is_err() {
                return;
            }
        }
    } else {
        fail(&mut stream, &format!("{service}: not recorded")).await;
    }
}

async fn shell(mut stream: TcpStream, shared: &Shared, serial: &str, command: &str) {
    let lookup = {
        let mut state = shared.state.lock().unwrap();
        state.calls.push(format!("{serial}: {command}"));
        if let Some(message) = state
            .failing
            .clone()
            .or_else(|| state.refusals.get(command).cloned())
        {
            Lookup::Fail(message)
        } else if let Some(read) = part_id(command).map(|id| state.parts.get(&id).cloned()) {
            Lookup::Part(read.unwrap_or(PartRead::Output(Answer::default())))
        } else if let Some(chunks) = state.streams.get(command) {
            Lookup::Stream(chunks.clone())
        } else if let Some(answer) = state.answers.get(command).or_else(|| {
            state
                .prefixes
                .iter()
                .find(|prefixed| command.starts_with(prefixed.prefix.as_str()))
                .map(|prefixed| &prefixed.answer)
        }) {
            Lookup::Answer(answer.clone())
        } else {
            Lookup::Fail(format!("no recorded output for: {command}"))
        }
    };
    let mut held = shared.hold.clone();
    if held.wait_for(|held| !*held).await.is_err() {
        return;
    }
    match lookup {
        Lookup::Fail(message) => fail(&mut stream, &message).await,
        Lookup::Answer(answer) | Lookup::Part(PartRead::Output(answer)) => {
            let mut bytes = b"OKAY".to_vec();
            bytes.extend(packet(1, &answer.stdout));
            if !answer.stderr.is_empty() {
                bytes.extend(packet(2, &answer.stderr));
            }
            bytes.extend(packet(3, &[answer.exit_code]));
            write(&mut stream, &bytes).await;
        }
        Lookup::Part(PartRead::NoExitPacket) => {
            let mut bytes = b"OKAY".to_vec();
            bytes.extend(packet(1, b"partial"));
            write(&mut stream, &bytes).await;
        }
        Lookup::Part(PartRead::ExitWithoutCode) => {
            let mut bytes = b"OKAY".to_vec();
            bytes.extend(packet(1, b"partial"));
            bytes.extend(packet(3, &[]));
            write(&mut stream, &bytes).await;
        }
        Lookup::Part(PartRead::Refused(message)) => fail(&mut stream, message).await,
        Lookup::Part(PartRead::Hang) => {
            write(&mut stream, b"OKAY").await;
            let (mut reader, _writer) = stream.split();
            closed(&mut reader).await;
        }
        Lookup::Stream(chunks) => {
            write(&mut stream, b"OKAY").await;
            for chunk in chunks {
                write(&mut stream, &packet(1, chunk.as_bytes())).await;
            }
            let (mut reader, _writer) = stream.split();
            closed(&mut reader).await;
        }
    }
}

enum Lookup {
    Fail(String),
    Answer(Answer),
    Part(PartRead),
    Stream(Vec<String>),
}

/// The part id of a `content read --uri content://mms/part/<id>`.
fn part_id(command: &str) -> Option<i64> {
    command
        .strip_prefix("content read --uri content://mms/part/")?
        .parse()
        .ok()
}

async fn receive_push(stream: &mut TcpStream, serial: &str) -> Option<Push> {
    let mut target = String::new();
    let mut size = 0;
    loop {
        let mut header = [0; 8];
        stream.read_exact(&mut header).await.ok()?;
        let length = usize::try_from(u32::from_le_bytes(header[4..].try_into().ok()?)).ok()?;
        match &header[..4] {
            b"SEND" => {
                let mut body = vec![0; length];
                stream.read_exact(&mut body).await.ok()?;
                target = String::from_utf8(body).ok()?;
            }
            b"DATA" => {
                let mut body = vec![0; length];
                stream.read_exact(&mut body).await.ok()?;
                size += length;
            }
            b"DONE" => {
                write(stream, b"OKAY\0\0\0\0").await;
                let (remote, mode) = target.rsplit_once(',')?;
                return Some(Push {
                    serial: serial.to_owned(),
                    remote: remote.to_owned(),
                    mode: mode.parse().ok()?,
                    size,
                });
            }
            _ => return None,
        }
    }
}

fn packet(id: u8, data: &[u8]) -> Vec<u8> {
    let mut bytes = vec![id];
    bytes.extend_from_slice(&u32::try_from(data.len()).unwrap().to_le_bytes());
    bytes.extend_from_slice(data);
    bytes
}

async fn read_request(stream: &mut TcpStream) -> Option<String> {
    let mut length = [0; 4];
    stream.read_exact(&mut length).await.ok()?;
    let length = usize::from_str_radix(str::from_utf8(&length).ok()?, 16).ok()?;
    let mut body = vec![0; length];
    stream.read_exact(&mut body).await.ok()?;
    String::from_utf8(body).ok()
}

/// Finishes when the client closes its side or the connection fails.
async fn closed(reader: &mut ReadHalf<'_>) {
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

/// Provider `_id`s in the B30's recorded inbox.
pub(crate) const RECORDED_IDS: [i64; 8] = [4, 5, 6, 7, 8, 9, 10, 11];

/// What `content query` prints when no row matches.
pub(crate) const NO_RESULT: &str = "No result found.\n";

/// `text` as SQLite's `hex()` prints it, for stand-in provider rows.
pub(crate) fn hex(text: &str) -> String {
    let digits: Vec<String> = text.bytes().map(|byte| format!("{byte:02X}")).collect();
    digits.concat()
}

impl FakeAdb {
    /// A B30 stand-in that also answers text ingest's commands with the B30's recorded provider
    /// and isms output. Queries for the same rows in another form, by id rather than after a
    /// cursor, get the same recording. New texts and MMS parts can't be produced on demand (the
    /// phone is out of service and its MMS inbox is empty), so tests add them as stand-ins.
    pub(crate) async fn sms() -> Self {
        use spareline_core::{Provider, ProviderQuery};

        let adb = Self::b30().await;
        let inbox = b30_fixture("sms-provider.txt");
        let sms = Provider::Sms;
        let mms = Provider::Mms;
        adb.answer(
            spareline_core::ISMS_TRIGGER_COMMAND,
            &b30_fixture("isms.txt"),
        );
        adb.answer(
            &ProviderQuery::Keys { provider: sms }.command(),
            &b30_fixture("sms-keys.txt"),
        );
        adb.answer(
            &ProviderQuery::After {
                provider: sms,
                after_id: 0,
            }
            .command(),
            &inbox,
        );
        adb.answer(
            &ProviderQuery::ByIds {
                provider: sms,
                ids: RECORDED_IDS.to_vec(),
            }
            .command(),
            &inbox,
        );
        adb.answer(
            &ProviderQuery::After {
                provider: sms,
                after_id: 11,
            }
            .command(),
            NO_RESULT,
        );
        adb.answer(
            &ProviderQuery::Keys { provider: mms }.command(),
            &b30_fixture("mms-keys.txt"),
        );
        adb.answer(
            &ProviderQuery::After {
                provider: mms,
                after_id: 0,
            }
            .command(),
            &b30_fixture("mms-provider.txt"),
        );
        adb
    }
}

/// A new, empty directory under the system's temporary directory, removed with its contents
/// when dropped.
pub(crate) struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    pub(crate) fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::{env, process};

        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = env::temp_dir().join(format!(
            "spareline-server-test-{}-{}",
            process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).expect("a new temporary directory can be created");
        Self { path }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.path) {
            assert_eq!(
                error.kind(),
                io::ErrorKind::NotFound,
                "cannot remove {}: {error}",
                self.path.display()
            );
        }
    }
}
