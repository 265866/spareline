//! The interface the rest of the service uses for phones.

use std::pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, UNIX_EPOCH};

use spareline_adb::{AdbClient, ShellOutput};
use tokio::sync::Notify;
use tokio::sync::broadcast::{self, Receiver, Sender};

use crate::clock::Clock;
use crate::devices::adb_status::AdbStatus;
use crate::devices::device_error::DeviceError;
use crate::devices::live_phone::LivePhone;
use crate::devices::phone_event::PhoneEvent;
use crate::devices::phone_shell::PhoneShell;
use crate::devices::phone_stream::PhoneStream;
use crate::devices::phone_table::PhoneTable;
use crate::devices::probe_reason::ProbeReason;
use crate::devices::session::SessionToken;

/// How long a push may take: scrcpy-server and the lock-screen program are a few hundred KB.
pub const PUSH_TIMEOUT: Duration = Duration::from_mins(2);

/// How many phone events a subscription may fall behind before it misses some.
const EVENT_CAPACITY: usize = 256;

/// The phones, as spareline-old's `Devices` showed them: what the phone manager knows of each
/// one, the adb server's reachability, events when phones come and go, and calls to the phones
/// that are online.
///
/// Every call to a phone goes through the phone's session: it fails with
/// [`DeviceError::NotOnline`] when the phone isn't online, and is cancelled with
/// [`DeviceError::Disconnected`] when the phone goes offline during it. Each call opens its own
/// connection to the adb server and is bounded by the timeout it is given; dropping its future
/// closes that connection.
///
/// Cloning is cheap; every clone shows the same phones.
#[derive(Clone, Debug)]
pub struct Devices {
    #[cfg_attr(
        dylint_lib = "trellis_lints",
        expect(
            shared_ownership,
            reason = "the manager's tasks write the one phone table that every route, stream \
                      and background task reads through its own clone of the handle"
        )
    )]
    shared: Arc<Shared>,
}

#[derive(Debug)]
struct Shared {
    adb: AdbClient,
    clock: Clock,
    table: Mutex<PhoneTable>,
    events: Sender<PhoneEvent>,
    probes_done: Notify,
}

impl Devices {
    /// A handle on no phones yet, which reaches phones through `adb` and reads the time from
    /// `clock`. The phone manager fills it; without one, no phone is ever online.
    #[must_use]
    pub fn new(adb: AdbClient, clock: Clock) -> Self {
        let table = PhoneTable::new(clock.now());
        Self {
            shared: Arc::new(Shared {
                adb,
                clock,
                table: Mutex::new(table),
                events: broadcast::channel(EVENT_CAPACITY).0,
                probes_done: Notify::new(),
            }),
        }
    }

    /// Every phone the manager has seen this run, in the order it first saw them.
    #[must_use]
    pub fn list(&self) -> Vec<LivePhone> {
        self.table().list()
    }

    /// The phone with `serial`, when the manager has seen it this run.
    #[must_use]
    pub fn get(&self, serial: &str) -> Option<LivePhone> {
        self.table().get(serial)
    }

    /// The adb server's reachability.
    #[must_use]
    pub fn adb(&self) -> AdbStatus {
        self.table().adb()
    }

    /// The phone events published from now on. A subscriber that falls more than 256 events
    /// behind gets [`RecvError::Lagged`](tokio::sync::broadcast::error::RecvError::Lagged) and
    /// should read [`Devices::list`] again.
    #[must_use]
    pub fn subscribe(&self) -> Receiver<PhoneEvent> {
        self.shared.events.subscribe()
    }

    /// Runs `command` on the phone through `sh -c` under the shell protocol and returns stdout,
    /// stderr and the exit code apart, for text output.
    ///
    /// # Errors
    ///
    /// [`DeviceError`] when the phone isn't online or disconnects, the call takes longer than
    /// `timeout`, or the adb server fails it. A non-zero exit code isn't an error.
    pub async fn shell(
        &self,
        serial: &str,
        command: &str,
        timeout: Duration,
    ) -> Result<ShellOutput, DeviceError> {
        let token = self.token(serial)?;
        self.shell_in(&token, serial, command, timeout).await
    }

    /// Runs `command` on the phone and returns its raw stdout, binary-safe. adbd merges stderr
    /// into it and reports no exit status; use [`Devices::shell`] when that matters.
    ///
    /// # Errors
    ///
    /// [`DeviceError`] when the phone isn't online or disconnects, the call takes longer than
    /// `timeout`, or the adb server fails it.
    pub async fn exec(
        &self,
        serial: &str,
        command: &str,
        timeout: Duration,
    ) -> Result<Vec<u8>, DeviceError> {
        let token = self.token(serial)?;
        let adb = self.adb_within(timeout);
        Ok(token.guard(serial, adb.exec(serial, command)).await??)
    }

    /// Writes `data` to `remote_path` on the phone as a regular file with permission bits
    /// `mode`, such as `0o644`, within [`PUSH_TIMEOUT`].
    ///
    /// # Errors
    ///
    /// [`DeviceError`] when the phone isn't online or disconnects, refuses the path, or the push
    /// takes longer than [`PUSH_TIMEOUT`].
    pub async fn push(
        &self,
        serial: &str,
        data: &[u8],
        remote_path: &str,
        mode: u32,
    ) -> Result<(), DeviceError> {
        let token = self.token(serial)?;
        let adb = self.adb_within(PUSH_TIMEOUT);
        let millis = u64::try_from(self.shared.clock.now().millis()).unwrap_or(0);
        let modified = UNIX_EPOCH + Duration::from_millis(millis);
        Ok(token
            .guard(serial, adb.push(serial, data, remote_path, mode, modified))
            .await??)
    }

    /// Opens the abstract unix socket `name` on the phone, such as the one scrcpy-server listens
    /// on. `timeout` bounds only the setup.
    ///
    /// # Errors
    ///
    /// [`DeviceError`] when the phone isn't online or disconnects, nothing listens there, or the
    /// setup takes longer than `timeout`.
    pub async fn open_local(
        &self,
        serial: &str,
        name: &str,
        timeout: Duration,
    ) -> Result<PhoneStream, DeviceError> {
        self.open_stream(serial, &format!("localabstract:{name}"), timeout)
            .await
    }

    /// Opens a duplex stream service on the phone, such as `localabstract:<name>` or
    /// `tcp:<port>`. `timeout` bounds only the setup; the stream stays open until either side
    /// closes it or the phone disconnects.
    ///
    /// # Errors
    ///
    /// [`DeviceError`] when the phone isn't online or disconnects, the service refuses, or the
    /// setup takes longer than `timeout`.
    pub async fn open_stream(
        &self,
        serial: &str,
        service: &str,
        timeout: Duration,
    ) -> Result<PhoneStream, DeviceError> {
        let token = self.token(serial)?;
        let adb = self.adb_within(timeout);
        let stream = token
            .guard(serial, adb.open_stream(serial, service))
            .await??;
        Ok(PhoneStream::new(stream, serial, token))
    }

    /// Starts `command`, such as `logcat`, under the shell protocol and streams its stdout. The
    /// stream ends when the command exits, the phone disconnects, or it is dropped, which has
    /// adbd end the command. `timeout` bounds only the setup.
    ///
    /// # Errors
    ///
    /// [`DeviceError`] when the phone isn't online or disconnects, or the setup takes longer
    /// than `timeout`.
    pub async fn open_shell(
        &self,
        serial: &str,
        command: &str,
        timeout: Duration,
    ) -> Result<PhoneShell, DeviceError> {
        let token = self.token(serial)?;
        let adb = self.adb_within(timeout);
        let stream = token
            .guard(serial, adb.shell_stream(serial, command))
            .await??;
        Ok(PhoneShell::new(stream, serial, token))
    }

    /// Reads the phone's identity (its SIMs and numbers) again now rather than at the next
    /// health round, for example after a SIM change. It runs after the probe in flight, if any.
    /// Does nothing for a phone that isn't online.
    pub fn refresh(&self, serial: &str) {
        self.table().schedule(serial, ProbeReason::Identity);
    }

    /// Runs `command` within the session `token` follows, as [`Devices::shell`] does, for the
    /// manager's probes, which belong to a session rather than to whatever session the phone
    /// has now, and for callers that need to tell whether a failed call's session ended.
    pub(crate) async fn shell_in(
        &self,
        token: &SessionToken,
        serial: &str,
        command: &str,
        timeout: Duration,
    ) -> Result<ShellOutput, DeviceError> {
        let adb = self.adb_within(timeout);
        Ok(token.guard(serial, adb.shell(serial, command)).await??)
    }

    /// The table, locked for one change or read. Never held across an `await`.
    pub(crate) fn table(&self) -> MutexGuard<'_, PhoneTable> {
        self.shared
            .table
            .lock()
            .expect("no task panicked while holding the phone table")
    }

    /// The clock the manager reads.
    pub(crate) fn clock(&self) -> &Clock {
        &self.shared.clock
    }

    /// The adb client.
    pub(crate) fn adb_client(&self) -> &AdbClient {
        &self.shared.adb
    }

    /// Publishes `event` to every subscription. Nobody listening isn't a failure.
    pub(crate) fn emit(&self, event: PhoneEvent) {
        match self.shared.events.send(event) {
            Ok(_subscribers) => {}
            Err(_nobody_listening) => {}
        }
    }

    /// Wakes whoever waits in [`Devices::probes_settled`].
    pub(crate) fn probe_done(&self) {
        self.shared.probes_done.notify_waiters();
    }

    /// Finishes once no phone has a probe queued or running.
    pub(crate) async fn probes_settled(&self) {
        loop {
            let mut done = pin::pin!(self.shared.probes_done.notified());
            done.as_mut().enable();
            if !self.table().any_probe_pending() {
                return;
            }
            done.await;
        }
    }

    /// A token of the session of the online phone `serial`, for calls through
    /// [`Devices::shell_in`] whose caller needs to tell afterwards whether that session ended.
    ///
    /// # Errors
    ///
    /// [`DeviceError::NotOnline`] when the phone isn't online.
    pub(crate) fn token(&self, serial: &str) -> Result<SessionToken, DeviceError> {
        self.table().token(serial)
    }

    fn adb_within(&self, timeout: Duration) -> AdbClient {
        self.shared.adb.clone().with_call_timeout(timeout)
    }
}

#[cfg(test)]
mod tests {
    use std::io::ErrorKind;

    use spareline_db::PhoneState;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::sync::mpsc::{self, UnboundedReceiver};
    use tokio::task;

    use super::*;
    use crate::fake_adb::{self, Answer, B30, FakeAdb, PartRead, Push};

    const TIMEOUT: Duration = Duration::from_secs(30);

    /// Puts `serial` online with a session, as the manager does, and returns its probe queue.
    fn online(devices: &Devices, serial: &str) -> UnboundedReceiver<ProbeReason> {
        let now = devices.clock().now();
        let mut table = devices.table();
        table.apply_state(serial, PhoneState::Device, Some("1-1"), now);
        let (sender, receiver) = mpsc::unbounded_channel();
        table.start_session(serial, sender).unwrap();
        receiver
    }

    struct Harness {
        adb: FakeAdb,
        devices: Devices,
    }

    async fn devices() -> Harness {
        let adb = FakeAdb::b30().await;
        let devices = Devices::new(adb.client(), Clock::tokio(fake_adb::START));
        Harness { adb, devices }
    }

    #[test]
    fn calls_reach_an_online_phone_through_its_session() {
        fake_adb::block_on(async {
            let Harness { adb, devices } = devices().await;
            let _probes = online(&devices, B30);
            adb.answer("getprop ro.product.model", "S6702X\n");
            let output = devices
                .shell(B30, "getprop ro.product.model", TIMEOUT)
                .await
                .unwrap();
            assert_eq!(output.stdout_text(), "S6702X\n");
            assert_eq!(output.exit_code, 0);
            let exec = devices.exec(B30, "true", TIMEOUT).await.unwrap();
            assert_eq!(exec, b"exec-ok");
            devices
                .push(B30, &[0; 10], "/data/local/tmp/x", 0o644)
                .await
                .unwrap();
            assert_eq!(
                adb.pushes(),
                [Push {
                    serial: B30.to_owned(),
                    remote: "/data/local/tmp/x".to_owned(),
                    mode: 33188,
                    size: 10,
                }]
            );
            assert_eq!(adb.exec_calls(), [format!("{B30}: true")]);
        });
    }

    #[test]
    fn a_phone_that_is_not_online_takes_no_calls() {
        fake_adb::block_on(async {
            let Harness { adb, devices } = devices().await;
            let error = devices.exec("NEWPHONE", "id", TIMEOUT).await.unwrap_err();
            assert_eq!(error.to_string(), "phone NEWPHONE is not online (unknown)");
            let now = devices.clock().now();
            devices
                .table()
                .apply_state("NEWPHONE", PhoneState::Unauthorized, None, now);
            let error = devices.shell("NEWPHONE", "id", TIMEOUT).await.unwrap_err();
            assert_eq!(
                error,
                DeviceError::NotOnline {
                    serial: "NEWPHONE".to_owned(),
                    state: "unauthorized".to_owned(),
                }
            );
            assert!(adb.calls().is_empty(), "nothing reached the adb server");
        });
    }

    #[test]
    fn a_call_in_flight_is_cancelled_when_the_phone_disconnects() {
        fake_adb::block_on(async {
            let Harness { adb, devices } = devices().await;
            let _probes = online(&devices, B30);
            adb.hold();
            let calling = devices.clone();
            let call = task::spawn(async move {
                calling
                    .shell(B30, &spareline_core::health_command(), TIMEOUT)
                    .await
            });
            fake_adb::until("the call to reach the phone", || !adb.calls().is_empty()).await;
            assert!(devices.table().end_session(B30));
            assert_eq!(
                call.await.unwrap(),
                Err(DeviceError::Disconnected {
                    serial: B30.to_owned()
                })
            );
        });
    }

    #[test]
    fn a_shell_stream_ends_when_the_phone_disconnects() {
        fake_adb::block_on(async {
            let Harness { adb, devices } = devices().await;
            let _probes = online(&devices, B30);
            adb.answer_stream("logcat -b radio", &["first line\n", "second line\n"]);
            let mut shell = devices
                .open_shell(B30, "logcat -b radio", TIMEOUT)
                .await
                .unwrap();
            assert_eq!(shell.serial(), B30);
            assert_eq!(shell.next_stdout().await.unwrap().unwrap(), b"first line\n");
            assert_eq!(
                shell.next_stdout().await.unwrap().unwrap(),
                b"second line\n"
            );
            devices.table().end_session(B30);
            assert_eq!(
                shell.next_stdout().await,
                Err(DeviceError::Disconnected {
                    serial: B30.to_owned()
                })
            );
        });
    }

    #[test]
    fn a_stream_carries_bytes_both_ways_until_the_phone_disconnects() {
        fake_adb::block_on(async {
            let Harness { devices, .. } = devices().await;
            let _probes = online(&devices, B30);
            let mut stream = devices.open_local(B30, "echo", TIMEOUT).await.unwrap();
            stream.write_all(b"ping").await.unwrap();
            let mut echoed = [0; 4];
            stream.read_exact(&mut echoed).await.unwrap();
            assert_eq!(&echoed, b"ping");
            devices.table().end_session(B30);
            let error = stream.read(&mut echoed).await.unwrap_err();
            assert_eq!(error.kind(), ErrorKind::ConnectionAborted);
            assert_eq!(error.to_string(), format!("phone {B30} disconnected"));
            let refused = devices.open_local(B30, "echo", TIMEOUT).await.unwrap_err();
            assert!(
                matches!(refused, DeviceError::NotOnline { .. }),
                "{refused:?}"
            );
        });
    }

    #[test]
    fn a_shell_that_ends_without_its_exit_code_is_an_error_not_output() {
        fake_adb::block_on(async {
            let Harness { adb, devices } = devices().await;
            let _probes = online(&devices, B30);
            adb.set_part(31, PartRead::NoExitPacket);
            let error = devices
                .shell(B30, "content read --uri content://mms/part/31", TIMEOUT)
                .await
                .unwrap_err();
            assert!(
                error
                    .to_string()
                    .starts_with("the shell ended without reporting an exit code"),
                "{error}"
            );
            adb.set_part(
                31,
                PartRead::Output(Answer {
                    stdout: b"partial".to_vec(),
                    stderr: b"Error while accessing provider:mms\n".to_vec(),
                    exit_code: 0,
                }),
            );
            let output = devices
                .shell(B30, "content read --uri content://mms/part/31", TIMEOUT)
                .await
                .unwrap();
            assert_eq!(output.stdout, b"partial");
            assert_eq!(output.stderr_text(), "Error while accessing provider:mms\n");
        });
    }

    #[test]
    fn a_service_the_phone_refuses_fails_with_its_reason() {
        fake_adb::block_on(async {
            let Harness { devices, .. } = devices().await;
            let _probes = online(&devices, B30);
            let error = devices
                .open_local(B30, "scrcpy", TIMEOUT)
                .await
                .unwrap_err();
            assert!(matches!(error, DeviceError::Adb(_)), "{error:?}");
            assert!(
                error
                    .to_string()
                    .contains("localabstract:scrcpy: not recorded"),
                "{error}"
            );
        });
    }

    #[test]
    fn subscribers_see_events_and_refresh_queues_an_identity_probe_for_an_online_phone() {
        fake_adb::block_on(async {
            let Harness { devices, .. } = devices().await;
            let mut events = devices.subscribe();
            devices.refresh(B30);
            let mut probes = online(&devices, B30);
            devices.emit(PhoneEvent::Ready {
                serial: B30.to_owned(),
            });
            assert_eq!(events.recv().await.unwrap().serial(), B30);
            devices.refresh(B30);
            assert_eq!(probes.try_recv().unwrap(), ProbeReason::Identity);
            assert!(probes.try_recv().is_err(), "one probe");
            assert_eq!(devices.list().len(), 1);
            assert_eq!(devices.get(B30).unwrap().usb.as_deref(), Some("1-1"));
            assert!(!devices.adb().reachable, "nothing reached adb yet");
        });
    }
}
