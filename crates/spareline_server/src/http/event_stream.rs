//! `GET /api/v1/events`: device events and texts as Server-Sent Events.

use std::convert::Infallible;
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::{RawQuery, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, header};
use axum::response::Response;
use axum::{Extension, Router, routing};
use futures_util::Stream;
use futures_util::stream::{self, BoxStream, StreamExt as _};
use serde::Serialize;
use spareline_db::{DatabaseError, DeviceEvent, Message, Principal};
use tokio::sync::broadcast::Receiver;
use tokio::sync::broadcast::error::RecvError;
use tokio::time::{self, Instant, MissedTickBehavior};

use crate::api_error::ApiError;
use crate::app_state::AppState;
use crate::http::query_params::{IntRange, QueryParams};
use crate::http::revocation::Revocation;
use crate::stream_cursor::{self, StreamCursor, StreamKinds};

/// How many events one replay query reads.
const REPLAY_PAGE: i64 = 500;

/// How often an idle stream sends a `: keep-alive` comment, so proxies keep it open.
pub(crate) const HEARTBEAT: Duration = Duration::from_secs(15);

/// The route of the event stream.
pub(crate) fn routes() -> Router<AppState> {
    Router::new().route("/api/v1/events", routing::get(events))
}

/// `GET /api/v1/events?types=device,message&after=<cursor>&latest=<n>`.
///
/// The stream replays what came after the cursor (`Last-Event-ID` wins over `after`), oldest
/// first, device events before texts, or without a device cursor the newest `latest` device
/// events; then sends `retry: 3000` with the current cursor once; then live events, and a
/// keep-alive comment every 15 s. It ends when the client goes away, its principal is signed
/// out, or the service stops.
async fn events(
    State(state): State<AppState>,
    principal: Option<Extension<Principal>>,
    headers: HeaderMap,
    RawQuery(raw): RawQuery,
) -> Response {
    use axum::response::IntoResponse as _;

    let query = QueryParams::parse(raw.as_deref());
    let request = match EventRequest::parse(&query, &headers) {
        Ok(request) => request,
        Err(error) => return error.into_response(),
    };
    let kinds = request.kinds;
    let device_events = kinds.device.then(|| state.buses().device_events());
    let texts = kinds.message.then(|| state.buses().texts());
    let revocation = principal
        .map(|found| found.0)
        .map(|principal| Revocation::watch(&state, principal));
    let cursor = request.cursor;
    let start = state
        .db()
        .call(move |db| -> Result<Position, DatabaseError> {
            let device = match cursor.device {
                Some(device) => device,
                None => db.device_events().latest_id()?,
            };
            let message = match cursor.message {
                Some(message) => message,
                None => db.messages().max_seq()?,
            };
            Ok(Position { device, message })
        })
        .await;
    let position = match start {
        Ok(position) => position,
        Err(error) => return ApiError::internal(state.log(), &error).into_response(),
    };
    let mut inputs: Vec<BoxStream<'static, Input>> = vec![heartbeats().boxed()];
    if let Some(receiver) = device_events {
        inputs.push(device_inputs(receiver).boxed());
    }
    if let Some(receiver) = texts {
        inputs.push(text_inputs(receiver).boxed());
    }
    if let Some(revocation) = revocation {
        inputs.push(
            stream::once(revocation.revoked())
                .map(|()| Input::Ended)
                .boxed(),
        );
    }
    inputs.push(
        stream::once(state.stopping())
            .map(|()| Input::Ended)
            .boxed(),
    );
    let replay = if cursor.device.is_some() {
        Phase::DeviceReplay
    } else if request.latest > 0 {
        Phase::DeviceLatest(request.latest)
    } else {
        Phase::TextReplay
    };
    let machine = EventMachine {
        state,
        kinds,
        position,
        replay_texts: cursor.message.is_some(),
        catch_up: CatchUp::default(),
        phase: if kinds.device {
            replay
        } else {
            Phase::TextReplay
        },
        inputs: stream::select_all(inputs).boxed(),
    };
    let body = stream::unfold(machine, |mut machine| async move {
        let chunk = machine.next_chunk().await?;
        Some((Ok::<Bytes, Infallible>(Bytes::from(chunk)), machine))
    });
    (
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/event-stream"),
            ),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-cache")),
            (
                HeaderName::from_static("x-accel-buffering"),
                HeaderValue::from_static("no"),
            ),
        ],
        Body::from_stream(body),
    )
        .into_response()
}

/// The query of `/api/v1/events`, checked.
struct EventRequest {
    kinds: StreamKinds,
    cursor: StreamCursor,
    latest: i64,
}

impl EventRequest {
    /// Reads `latest` (0-500), `after`, `types` and the `Last-Event-ID` header, in the order
    /// spareline-old checked them: the query's form first (422), then the types (400). A
    /// repeated `latest` or `after` takes its first value, and repeated `types` are joined with
    /// `,` before they are checked, as the live build read them.
    fn parse(query: &QueryParams, headers: &HeaderMap) -> Result<Self, ApiError> {
        let latest = query
            .integer("latest", IntRange::between(0, 500))?
            .unwrap_or(0);
        let after = query.first("after");
        let kinds = StreamKinds::parse(&[query.all("types").join(",")])?;
        let last_event_id = headers
            .get("last-event-id")
            .map(|value| String::from_utf8_lossy(value.as_bytes()).into_owned());
        let raw = last_event_id.as_deref().or(after);
        Ok(Self {
            kinds,
            cursor: StreamCursor::parse(raw, kinds),
            latest,
        })
    }
}

/// The last device event id and text seq a stream has sent, or started from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Position {
    device: i64,
    message: i64,
}

/// What wakes a live stream.
enum Input {
    /// A device event was published.
    Device(DeviceEvent),
    /// The stream fell behind the device events published; read them from the database.
    DeviceLagged,
    /// A text was published.
    Text(Message),
    /// The stream fell behind the texts published; read them from the database.
    TextLagged,
    /// Time for a keep-alive comment.
    Heartbeat,
    /// The principal was signed out, or the service is stopping.
    Ended,
}

/// What a live stream does with one input.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Reply {
    /// Sends this chunk.
    Send(String),
    /// Sends nothing now.
    Nothing,
    /// Ends the stream.
    End,
}

/// Which streams a live stream must read from the database before it waits for more input,
/// because the bus skipped ahead, delivered out of order, or dropped events for it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct CatchUp {
    devices: bool,
    texts: bool,
}

/// Where a stream is in its output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// Replaying device events after the cursor, a page at a time.
    DeviceReplay,
    /// Replaying the newest this many device events.
    DeviceLatest(i64),
    /// Replaying texts after the cursor, when there is a text cursor.
    TextReplay,
    /// Sending `retry: 3000` and the current cursor.
    Opening,
    /// Sending live events.
    Live,
}

/// One stream's state, which writes its output a chunk at a time.
struct EventMachine {
    state: AppState,
    kinds: StreamKinds,
    position: Position,
    replay_texts: bool,
    catch_up: CatchUp,
    phase: Phase,
    inputs: BoxStream<'static, Input>,
}

impl EventMachine {
    /// The next chunk to send, or `None` to end the stream.
    async fn next_chunk(&mut self) -> Option<String> {
        match self.produce().await {
            Ok(chunk) => chunk,
            Err(error) => {
                self.state
                    .log()
                    .event("request-failed")
                    .field("error", &error.to_string())
                    .write();
                None
            }
        }
    }

    async fn produce(&mut self) -> Result<Option<String>, DatabaseError> {
        loop {
            if self.reads_a_page() && self.ended_between_pages() {
                return Ok(None);
            }
            match self.phase {
                Phase::DeviceReplay => {
                    let page = device_page(&self.state, self.position.device).await?;
                    let full = i64::try_from(page.len()).is_ok_and(|len| len >= REPLAY_PAGE);
                    if !full {
                        self.phase = Phase::TextReplay;
                    }
                    if let Some(chunk) = self.device_chunk(page) {
                        return Ok(Some(chunk));
                    }
                }
                Phase::DeviceLatest(count) => {
                    let events = self
                        .state
                        .db()
                        .call(move |db| db.device_events().latest(count))
                        .await?;
                    self.phase = Phase::TextReplay;
                    let newest = self.position.device;
                    self.position.device = 0;
                    let chunk = self.device_chunk(events);
                    self.position.device = self.position.device.max(newest);
                    if let Some(chunk) = chunk {
                        return Ok(Some(chunk));
                    }
                }
                Phase::TextReplay => {
                    if !self.replay_texts || !self.kinds.message {
                        self.phase = Phase::Opening;
                        continue;
                    }
                    let page = text_page(&self.state, self.position.message).await?;
                    let full = i64::try_from(page.len()).is_ok_and(|len| len >= REPLAY_PAGE);
                    if !full {
                        self.phase = Phase::Opening;
                    }
                    if let Some(chunk) = self.text_chunk(page) {
                        return Ok(Some(chunk));
                    }
                }
                Phase::Opening => {
                    self.phase = Phase::Live;
                    return Ok(Some(format!("retry: 3000\nid: {}\n\n", self.id())));
                }
                Phase::Live if self.catch_up.devices => {
                    let page = device_page(&self.state, self.position.device).await?;
                    self.catch_up.devices =
                        i64::try_from(page.len()).is_ok_and(|len| len >= REPLAY_PAGE);
                    if let Some(chunk) = self.device_chunk(page) {
                        return Ok(Some(chunk));
                    }
                }
                Phase::Live if self.catch_up.texts => {
                    let page = text_page(&self.state, self.position.message).await?;
                    self.catch_up.texts =
                        i64::try_from(page.len()).is_ok_and(|len| len >= REPLAY_PAGE);
                    if let Some(chunk) = self.text_chunk(page) {
                        return Ok(Some(chunk));
                    }
                }
                Phase::Live => {
                    let Some(input) = self.inputs.next().await else {
                        return Ok(None);
                    };
                    match self.live(input) {
                        Reply::Send(chunk) => return Ok(Some(chunk)),
                        Reply::Nothing => {}
                        Reply::End => return Ok(None),
                    }
                }
            }
        }
    }

    /// Whether the next step reads a page from the database: a replay, or a catch-up.
    fn reads_a_page(&self) -> bool {
        match self.phase {
            Phase::DeviceReplay | Phase::DeviceLatest(_) | Phase::TextReplay => true,
            Phase::Live => self.catch_up.devices || self.catch_up.texts,
            Phase::Opening => false,
        }
    }

    /// Takes the inputs that are ready without waiting, between two pages of a replay or a
    /// catch-up: `true` when the stream must end, because the service is stopping or the
    /// principal was signed out. A device event or text past the position starts a catch-up
    /// once the replay is done, since the pages read so far may not hold it.
    fn ended_between_pages(&mut self) -> bool {
        use futures_util::FutureExt as _;

        while let Some(ready) = self.inputs.next().now_or_never() {
            match ready {
                None | Some(Input::Ended) => return true,
                Some(Input::Device(event)) if event.id <= self.position.device => {}
                Some(Input::Text(text)) if text.seq <= self.position.message => {}
                Some(Input::Device(_) | Input::DeviceLagged) => self.catch_up.devices = true,
                Some(Input::Text(_) | Input::TextLagged) => self.catch_up.texts = true,
                Some(Input::Heartbeat) => {}
            }
        }
        false
    }

    /// What one live input sends. An event that isn't the next one starts a catch-up from the
    /// database, which [`EventMachine::produce`] sends a page of [`REPLAY_PAGE`] at a time.
    fn live(&mut self, input: Input) -> Reply {
        let chunk = match input {
            Input::Device(event) if event.id <= self.position.device => None,
            Input::Device(event) if event.id == self.position.device + 1 => {
                self.device_chunk(vec![event])
            }
            Input::Device(_) | Input::DeviceLagged => {
                self.catch_up.devices = true;
                None
            }
            Input::Text(text) if text.seq <= self.position.message => None,
            Input::Text(text) if text.seq == self.position.message + 1 => {
                self.text_chunk(vec![text])
            }
            Input::Text(_) | Input::TextLagged => {
                self.catch_up.texts = true;
                None
            }
            Input::Heartbeat => Some(": keep-alive\n\n".to_owned()),
            Input::Ended => return Reply::End,
        };
        chunk.map_or(Reply::Nothing, Reply::Send)
    }

    /// The events of `events` past the position, written out, moving the position.
    fn device_chunk(&mut self, events: Vec<DeviceEvent>) -> Option<String> {
        let mut chunk = String::new();
        for event in events {
            if event.id <= self.position.device {
                continue;
            }
            self.position.device = event.id;
            self.write_event(&mut chunk, "device", &event);
        }
        (!chunk.is_empty()).then_some(chunk)
    }

    /// The texts of `texts` past the position, written out, moving the position.
    fn text_chunk(&mut self, texts: Vec<Message>) -> Option<String> {
        let mut chunk = String::new();
        for text in texts {
            if text.seq <= self.position.message {
                continue;
            }
            self.position.message = text.seq;
            self.write_event(&mut chunk, "message", &text);
        }
        (!chunk.is_empty()).then_some(chunk)
    }

    /// Appends `id: <cursor>\nevent: <name>\ndata: <JSON>\n\n`, with the cursor after this event.
    fn write_event<T: Serialize>(&self, chunk: &mut String, name: &str, data: &T) {
        use std::fmt::Write as _;

        let json = serde_json::to_string(data).unwrap_or_else(|_| "null".to_owned());
        match write!(chunk, "id: {}\nevent: {name}\ndata: {json}\n\n", self.id()) {
            Ok(()) | Err(_) => {}
        }
    }

    fn id(&self) -> String {
        stream_cursor::event_id(self.kinds, self.position.device, self.position.message)
    }
}

async fn device_page(state: &AppState, after: i64) -> Result<Vec<DeviceEvent>, DatabaseError> {
    state
        .db()
        .call(move |db| db.device_events().after(after, REPLAY_PAGE))
        .await
}

async fn text_page(state: &AppState, after: i64) -> Result<Vec<Message>, DatabaseError> {
    state
        .db()
        .call(move |db| db.messages().after(after, REPLAY_PAGE))
        .await
}

/// A keep-alive tick every [`HEARTBEAT`], the first one [`HEARTBEAT`] from now.
fn heartbeats() -> impl Stream<Item = Input> + Send + 'static {
    let mut ticks = time::interval_at(Instant::now() + HEARTBEAT, HEARTBEAT);
    ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
    stream::unfold(ticks, |mut ticks| async move {
        ticks.tick().await;
        Some((Input::Heartbeat, ticks))
    })
}

/// The device events published from now on, as stream inputs.
fn device_inputs(receiver: Receiver<DeviceEvent>) -> impl Stream<Item = Input> + Send + 'static {
    stream::unfold(receiver, |mut receiver| async move {
        let input = match receiver.recv().await {
            Ok(event) => Input::Device(event),
            Err(RecvError::Lagged(_)) => Input::DeviceLagged,
            Err(RecvError::Closed) => return None,
        };
        Some((input, receiver))
    })
}

/// The texts published from now on, as stream inputs.
fn text_inputs(receiver: Receiver<Message>) -> impl Stream<Item = Input> + Send + 'static {
    stream::unfold(receiver, |mut receiver| async move {
        let input = match receiver.recv().await {
            Ok(text) => Input::Text(text),
            Err(RecvError::Lagged(_)) => Input::TextLagged,
            Err(RecvError::Closed) => return None,
        };
        Some((input, receiver))
    })
}
