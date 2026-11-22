//! One scrcpy-server run on one phone, from the settle to the teardown `pkill`.

use std::future::{self, Future};
use std::pin::{self, Pin};
use std::time::Duration;

use futures_util::future::{self as futures, Either};
use spareline_core::screen::{
    KILL_ANY_SERVER_COMMAND, LIST_ANY_SCRCPY_COMMAND, SERVER_JAR_PHONE_PATH, Scid, VideoFrame,
    VideoFramer, VideoMeta,
};
use tokio::io::{self, AsyncReadExt as _, ReadHalf, WriteHalf};
use tokio::sync::mpsc::{self, Receiver as WriteReceiver, Sender as WriteSender, UnboundedSender};
use tokio::sync::oneshot::{self, Receiver, Sender};
use tokio::time::{self, Instant};

use crate::devices::{Devices, PhoneStream};
use crate::event_log::EventLog;
use crate::screen::screen_command::{ScreenCommand, SessionEvent};
use crate::screen::task_guard::TaskGuard;
use crate::shell_text;

/// How long the settle waits for earlier scrcpy processes to exit.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(3);

/// How often the settle polls for scrcpy processes.
const SETTLE_POLL: Duration = Duration::from_millis(50);

/// The bound of each settle, wake and teardown command.
const SHORT_COMMAND: Duration = Duration::from_secs(5);

/// The bound of opening each socket to the server.
const SOCKET_SETUP: Duration = Duration::from_secs(5);

/// The wait between attempts at the video socket.
const CONNECT_RETRY: Duration = Duration::from_millis(100);

/// The bound of opening the stream that runs the server, which then stays open for the session.
const SERVER_SETUP: Duration = Duration::from_secs(30);

/// The most of the server's output a failure reason quotes, in characters.
const OUTPUT_TAIL: usize = 500;

/// How many control messages may wait for the control socket. More means the socket is stalled,
/// and the relay drops what comes rather than queue it without bound.
const WRITE_BACKLOG: usize = 256;

/// How much of the video socket one read takes.
const READ_CHUNK: usize = 64 * 1024;

/// The future of the stream that runs the server: it finishes, with the reason, only when the
/// server exits or the stream fails.
type ServerRun = Pin<Box<dyn Future<Output = String> + Send>>;

/// Everything a session's task needs.
#[derive(Debug)]
pub(crate) struct SessionSetup {
    /// The phones.
    pub(crate) devices: Devices,
    /// Where the session logs its start and end.
    pub(crate) log: EventLog,
    /// The phone's serial.
    pub(crate) serial: String,
    /// The session's id.
    pub(crate) scid: Scid,
    /// Which of the phone's sessions this is, for its events.
    pub(crate) generation: u64,
    /// The scrcpy-server jar to push.
    pub(crate) jar: &'static [u8],
    /// How long the start keeps trying the video socket.
    pub(crate) connect_timeout: Duration,
    /// Where the session's events go: the phone's relay.
    pub(crate) events: UnboundedSender<ScreenCommand>,
}

/// The relay's handle on a session's task. Dropping it stops the session.
#[derive(Debug)]
pub(crate) struct ScrcpySession {
    generation: u64,
    control: WriteSender<Vec<u8>>,
    stop: Option<Sender<()>>,
}

impl ScrcpySession {
    /// Starts a session's task. It reports [`SessionEvent::Started`] once the video header has
    /// arrived, each packet after, [`SessionEvent::Ended`] once, and [`SessionEvent::TornDown`]
    /// after its server has been killed. `guard` is held until then, so the service's shutdown
    /// waits for the kill.
    pub(crate) fn spawn(setup: SessionSetup, guard: TaskGuard) -> Self {
        let (control, writes) = mpsc::channel(WRITE_BACKLOG);
        let (stop, stopped) = oneshot::channel();
        let session = Self {
            generation: setup.generation,
            control,
            stop: Some(stop),
        };
        drop(tokio::spawn(run(setup, stopped, writes, guard)));
        session
    }

    /// Which of the phone's sessions this is.
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    /// Queues one control message for the server, in order. Callers validate it first. Returns
    /// whether it was queued: a message written after the session ended goes nowhere, and one
    /// that finds [`WRITE_BACKLOG`] messages still waiting for a stalled socket is dropped.
    pub(crate) fn write(&self, message: Vec<u8>) -> bool {
        self.control.try_send(message).is_ok()
    }

    /// Stops the session; its task still reports the end and the teardown.
    pub(crate) fn stop(&mut self) {
        if let Some(stop) = self.stop.take() {
            match stop.send(()) {
                Ok(()) | Err(()) => {}
            }
        }
    }
}

/// The session's whole life: the run, then the end and the teardown.
async fn run(
    setup: SessionSetup,
    stopped: Receiver<()>,
    writes: WriteReceiver<Vec<u8>>,
    guard: TaskGuard,
) {
    let error = drive(&setup, stopped, writes).await.err();
    setup
        .log
        .event("screen-session-ended")
        .field("serial", &setup.serial)
        .field("scid", &setup.scid.to_string())
        .field("error", &error)
        .write();
    send(&setup, SessionEvent::Ended(error));
    match setup
        .devices
        .exec(&setup.serial, &setup.scid.kill_command(), SHORT_COMMAND)
        .await
    {
        Ok(_) | Err(_) => {}
    }
    send(&setup, SessionEvent::TornDown);
    drop(guard);
}

fn send(setup: &SessionSetup, event: SessionEvent) {
    let command = ScreenCommand::Session {
        generation: setup.generation,
        event,
    };
    match setup.events.send(command) {
        Ok(()) | Err(_) => {}
    }
}

/// Runs the session until it is stopped, `Ok`, or fails, with the reason. Every socket and the
/// server's stream close when it returns.
async fn drive(
    setup: &SessionSetup,
    mut stopped: Receiver<()>,
    writes: WriteReceiver<Vec<u8>>,
) -> Result<(), String> {
    let begin = Instant::now();
    let settle_ms = {
        let settling = pin::pin!(settle(setup));
        match futures::select(&mut stopped, settling).await {
            Either::Left(_) => return Ok(()),
            Either::Right((settled, _)) => settled?,
        }
    };
    {
        let push = pin::pin!(setup.devices.push(
            &setup.serial,
            setup.jar,
            SERVER_JAR_PHONE_PATH,
            0o644
        ));
        match futures::select(&mut stopped, push).await {
            Either::Left(_) => return Ok(()),
            Either::Right((pushed, _)) => pushed.map_err(|error| error.to_string())?,
        }
    }
    let mut server: ServerRun = Box::pin(run_server(
        setup.devices.clone(),
        setup.serial.clone(),
        setup.scid,
    ));
    let opened = {
        let ending = pin::pin!(ended(&mut stopped, &mut server));
        let opening = pin::pin!(open(setup));
        match futures::select(ending, opening).await {
            Either::Left((end, _)) => return end,
            Either::Right((opened, _)) => opened?,
        }
    };
    let Opened {
        video,
        framer,
        control,
        meta,
    } = opened;
    setup
        .log
        .event("screen-session-started")
        .field("serial", &setup.serial)
        .field("scid", &setup.scid.to_string())
        .field("width", &meta.width)
        .field("height", &meta.height)
        .field("ms", &begin.elapsed().as_millis())
        .field("settleMs", &settle_ms)
        .write();
    send(setup, SessionEvent::Started(meta));
    let (control_reader, control_writer) = io::split(control);
    let pumping = pin::pin!(pump(setup, video, framer));
    let controlling = pin::pin!(run_control(control_reader, control_writer, writes));
    let streams = futures::select(pumping, controlling);
    let ending = pin::pin!(ended(&mut stopped, &mut server));
    match futures::select(ending, streams).await {
        Either::Left((end, _)) => end,
        Either::Right((streams_ended, _)) => Err(streams_ended.factor_first().0),
    }
}

/// Finishes when the session is stopped, `Ok`, or its server exits, with the reason.
async fn ended(stopped: &mut Receiver<()>, server: &mut ServerRun) -> Result<(), String> {
    match futures::select(stopped, server).await {
        Either::Left(_) => Ok(()),
        Either::Right((reason, _)) => Err(reason),
    }
}

/// Kills any scrcpy-server left on the phone and waits until no scrcpy process is left,
/// returning how long that took in milliseconds.
///
/// A server's `CleanUp` child restores `stay_on_while_plugged_in` after the server dies. A
/// server started while another runs, or before that `CleanUp` finishes, would record the changed
/// value as the one to restore. So a start kills any server on the phone, an orphan from a crashed
/// relay or someone's own scrcpy, and fails when it and its `CleanUp` don't go within 3 s.
async fn settle(setup: &SessionSetup) -> Result<u128, String> {
    let begin = Instant::now();
    let deadline = begin + SETTLE_TIMEOUT;
    exec(setup, KILL_ANY_SERVER_COMMAND).await?;
    loop {
        let output = exec(setup, LIST_ANY_SCRCPY_COMMAND).await?;
        let pids = shell_text::shell_text(&output);
        let pids = pids.trim();
        if pids.is_empty() {
            return Ok(begin.elapsed().as_millis());
        }
        if Instant::now() >= deadline {
            let listed: Vec<&str> = pids.split_whitespace().collect();
            return Err(format!(
                "scrcpy processes {} still running after {} ms",
                listed.join(", "),
                SETTLE_TIMEOUT.as_millis()
            ));
        }
        time::sleep(SETTLE_POLL).await;
    }
}

async fn exec(setup: &SessionSetup, command: &str) -> Result<Vec<u8>, String> {
    setup
        .devices
        .exec(&setup.serial, command, SHORT_COMMAND)
        .await
        .map_err(|error| error.to_string())
}

/// Runs the server and finishes only when it exits or its stream fails, with the reason.
/// Dropping the future closes the stream, which has adbd kill the server.
async fn run_server(devices: Devices, serial: String, scid: Scid) -> String {
    let service = format!("exec:{}", scid.server_command());
    let mut stream = match devices.open_stream(&serial, &service, SERVER_SETUP).await {
        Ok(stream) => stream,
        Err(error) => return error.to_string(),
    };
    let mut output = Vec::new();
    match stream.read_to_end(&mut output).await {
        Ok(_length) => format!(
            "scrcpy-server exited: {}",
            tail(&shell_text::shell_text(&output))
        ),
        Err(error) => error.to_string(),
    }
}

/// The end of the server's output, as a failure reason quotes it.
fn tail(output: &str) -> String {
    let trimmed = output.trim();
    let length = trimmed.chars().count();
    if length > OUTPUT_TAIL {
        let end: String = trimmed.chars().skip(length - OUTPUT_TAIL).collect();
        format!("\u{2026}{end}")
    } else if trimmed.is_empty() {
        "(no output)".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// The video socket once the server's dummy byte came, with the framer that read it.
struct VideoSocket {
    stream: PhoneStream,
    framer: VideoFramer,
}

/// The server's sockets and its video header, once a start has them.
struct Opened {
    video: PhoneStream,
    framer: VideoFramer,
    control: PhoneStream,
    meta: VideoMeta,
}

/// Opens the video socket, then the control socket, then reads the video header.
async fn open(setup: &SessionSetup) -> Result<Opened, String> {
    let VideoSocket {
        stream: mut video,
        mut framer,
    } = connect_video(setup).await?;
    let control = setup
        .devices
        .open_local(&setup.serial, &setup.scid.socket_name(), SOCKET_SETUP)
        .await
        .map_err(|error| error.to_string())?;
    let meta = read_meta(&mut video, &mut framer).await?;
    if !meta.is_h264() {
        return Err(format!("expected h264 video, got codec {}", meta.codec));
    }
    Ok(Opened {
        video,
        framer,
        control,
        meta,
    })
}

/// Connects to the server's video socket. The tunnel accepts a connection before the server
/// listens, so only the server's dummy byte proves the far end is live; until it comes, the
/// start retries every 100 ms for up to the connect timeout.
async fn connect_video(setup: &SessionSetup) -> Result<VideoSocket, String> {
    let deadline = Instant::now() + setup.connect_timeout;
    let mut last = String::from("no attempt was made");
    while Instant::now() < deadline {
        match time::timeout_at(deadline, video_attempt(setup)).await {
            Ok(Ok(connected)) => return Ok(connected),
            Ok(Err(error)) => last = error,
            Err(_elapsed) => {
                "the video socket gave no dummy byte in time".clone_into(&mut last);
                break;
            }
        }
        time::sleep(CONNECT_RETRY).await;
    }
    Err(format!(
        "scrcpy-server did not accept the video socket: {last}"
    ))
}

async fn video_attempt(setup: &SessionSetup) -> Result<VideoSocket, String> {
    let mut stream = setup
        .devices
        .open_local(&setup.serial, &setup.scid.socket_name(), SOCKET_SETUP)
        .await
        .map_err(|error| error.to_string())?;
    let mut dummy = [0; 1];
    let read = stream
        .read(&mut dummy)
        .await
        .map_err(|error| error.to_string())?;
    if read == 0 {
        return Err("stream ended after 0 of 1 expected bytes".to_owned());
    }
    let mut framer = VideoFramer::new();
    framer.push(&dummy);
    match framer.next_frame() {
        Ok(_nothing_until_the_header) => Ok(VideoSocket { stream, framer }),
        Err(error) => Err(error.to_string()),
    }
}

/// Reads until the framer has the video header.
async fn read_meta(video: &mut PhoneStream, framer: &mut VideoFramer) -> Result<VideoMeta, String> {
    let mut buffer = vec![0; READ_CHUNK];
    loop {
        match framer.next_frame().map_err(|error| error.to_string())? {
            Some(VideoFrame::Meta(meta)) => return Ok(meta),
            Some(VideoFrame::Packet(_)) => {
                return Err("a video packet came before the video header".to_owned());
            }
            None => {}
        }
        let read = video
            .read(&mut buffer)
            .await
            .map_err(|error| error.to_string())?;
        if read == 0 {
            framer.finish().map_err(|error| error.to_string())?;
            return Err("the video stream ended".to_owned());
        }
        framer.push(&buffer[..read]);
    }
}

/// Hands each packet to the relay as it completes, and finishes with the reason the video
/// stream ended.
async fn pump(setup: &SessionSetup, mut video: PhoneStream, mut framer: VideoFramer) -> String {
    let mut buffer = vec![0; READ_CHUNK];
    loop {
        loop {
            match framer.next_frame() {
                Ok(Some(VideoFrame::Packet(packet))) => send(setup, SessionEvent::Packet(packet)),
                Ok(Some(VideoFrame::Meta(_))) => {
                    return "a second video header came mid-stream".to_owned();
                }
                Ok(None) => break,
                Err(error) => return error.to_string(),
            }
        }
        match video.read(&mut buffer).await {
            Ok(0) => {
                return match framer.finish() {
                    Ok(()) => "the video stream ended".to_owned(),
                    Err(error) => error.to_string(),
                };
            }
            Ok(read) => framer.push(&buffer[..read]),
            Err(error) => return error.to_string(),
        }
    }
}

/// Runs the control socket: drains what the server sends and writes what the relay queues,
/// finishing with the reason the socket closed.
async fn run_control(
    from_server: ReadHalf<PhoneStream>,
    to_server: WriteHalf<PhoneStream>,
    writes: WriteReceiver<Vec<u8>>,
) -> String {
    let draining = pin::pin!(drain(from_server));
    let writing = pin::pin!(write_control(to_server, writes));
    match futures::select(draining, writing).await {
        Either::Left((reason, _)) => reason,
        Either::Right((never, _)) => match never {},
    }
}

/// Reads and discards what the server sends on the control socket, clipboard and UHID messages
/// the relay doesn't use, so the socket never fills; finishes when it closes.
async fn drain(mut control: ReadHalf<PhoneStream>) -> String {
    let mut buffer = vec![0; 4096];
    loop {
        match control.read(&mut buffer).await {
            Ok(0) => return "the control socket closed".to_owned(),
            Ok(_read) => {}
            Err(error) => return error.to_string(),
        }
    }
}

/// Writes the relay's control messages in order. A failed write is dropped: the socket is
/// broken, which the drain reports.
async fn write_control(
    mut control: WriteHalf<PhoneStream>,
    mut writes: WriteReceiver<Vec<u8>>,
) -> Never {
    use tokio::io::AsyncWriteExt as _;

    while let Some(message) = writes.recv().await {
        match control.write_all(&message).await {
            Ok(()) | Err(_) => {}
        }
    }
    future::pending().await
}

/// No value: what [`write_control`] returns, since it never finishes.
enum Never {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_server_output_is_quoted_by_its_last_500_characters() {
        let output = format!("{}{}", "a".repeat(10), "b".repeat(500));
        assert_eq!(tail(&output), format!("\u{2026}{}", "b".repeat(500)));
        assert_eq!(tail("  \n"), "(no output)");
        assert_eq!(tail(" Aborted \n"), "Aborted");
    }
}
