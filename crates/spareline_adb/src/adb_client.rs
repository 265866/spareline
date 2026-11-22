//! The client: one call, one connection to the adb server.

use std::future::Future;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::net::TcpStream;
use tokio::time;

use crate::adb_error::{AdbError, AdbErrorKind};
use crate::device_entry::DeviceEntry;
use crate::device_tracker::DeviceTracker;
use crate::endpoint::Endpoint;
use crate::frame;
use crate::shell_header::ShellHeader;
use crate::shell_output::ShellOutput;
use crate::shell_stream::ShellStream;

/// How long a request-and-response call may take, connection included, unless the caller sets
/// another deadline.
const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// How long connecting to the adb server may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// A client for one adb server. It holds no connection: each call opens its own and closes it
/// when the call ends, and dropping a call's future closes its connection.
#[derive(Clone, Debug)]
pub struct AdbClient {
    endpoint: Endpoint,
    call_timeout: Duration,
}

impl AdbClient {
    /// A client for the adb server at `endpoint`.
    #[must_use]
    pub fn new(endpoint: Endpoint) -> Self {
        Self {
            endpoint,
            call_timeout: DEFAULT_CALL_TIMEOUT,
        }
    }

    /// The same client with request-and-response calls bounded by `timeout` instead of 30 s.
    #[must_use]
    pub fn with_call_timeout(mut self, timeout: Duration) -> Self {
        self.call_timeout = timeout;
        self
    }

    /// The adb server's address.
    #[must_use]
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// The adb server's protocol version, such as 41.
    ///
    /// # Errors
    ///
    /// Returns an error when the server is unreachable or answers something else.
    pub async fn version(&self) -> Result<u32, AdbError> {
        self.bounded(async {
            let mut stream = self.connect().await?;
            frame::request(&mut stream, "host:version").await?;
            let text = frame::read_hex_string(&mut stream).await?;
            u32::from_str_radix(&text, 16).map_err(|_| {
                AdbError::new(
                    AdbErrorKind::Protocol,
                    format!("host:version: expected a hex version, got {text:?}"),
                )
            })
        })
        .await
    }

    /// Every transport the adb server sees.
    ///
    /// # Errors
    ///
    /// Returns an error when the server is unreachable or the call times out.
    pub async fn list_devices(&self) -> Result<Vec<DeviceEntry>, AdbError> {
        self.bounded(async {
            let mut stream = self.connect().await?;
            frame::request(&mut stream, "host:devices-l").await?;
            let payload = frame::read_hex_string(&mut stream).await?;
            Ok(DeviceEntry::parse_list(&payload))
        })
        .await
    }

    /// Follows the device list: the full list now and after every change. Only connecting and
    /// the request are bounded; the stream stays open until either side closes it.
    ///
    /// # Errors
    ///
    /// Returns an error when the server is unreachable or refuses the request.
    pub async fn track_devices(&self) -> Result<DeviceTracker, AdbError> {
        self.bounded(async {
            let mut stream = self.connect().await?;
            frame::request(&mut stream, "host:track-devices-l").await?;
            Ok(DeviceTracker::new(stream))
        })
        .await
    }

    /// Runs `command` through `sh -c` on the phone without a pty and returns stdout, stderr and
    /// the exit code apart (the `shell,v2` protocol).
    ///
    /// # Errors
    ///
    /// Returns an error when the phone isn't attached, the shell ends without an exit code, or
    /// the call times out. A non-zero exit code isn't an error.
    pub async fn shell(&self, serial: &str, command: &str) -> Result<ShellOutput, AdbError> {
        self.bounded(async {
            let service = format!("shell,v2,raw:{command}");
            let mut stream = self.open_device_service(serial, &service).await?;
            let mut output = ShellOutput::default();
            loop {
                let header = ShellHeader::read(&mut stream).await;
                let Some(ShellHeader { id, length }) = header.map_err(ended_early)? else {
                    return Err(ended_early(AdbError::new(
                        AdbErrorKind::Closed,
                        String::new(),
                    )));
                };
                let data = frame::read_exact(&mut stream, length).await?;
                frame::check_output(output.stdout.len() + output.stderr.len() + data.len())?;
                match id {
                    frame::SHELL_STDOUT => output.stdout.extend(data),
                    frame::SHELL_STDERR => output.stderr.extend(data),
                    frame::SHELL_EXIT => {
                        let Some(&code) = data.first() else {
                            return Err(AdbError::new(
                                AdbErrorKind::Protocol,
                                "shell exit packet without a code".to_owned(),
                            ));
                        };
                        output.exit_code = code;
                        return Ok(output);
                    }
                    _ => {}
                }
            }
        })
        .await
    }

    /// Runs `command` and returns its raw output, binary-safe. Without the shell protocol, adbd
    /// merges stderr into stdout and reports no exit status; use [`AdbClient::shell`] when that
    /// matters.
    ///
    /// # Errors
    ///
    /// Returns an error when the phone isn't attached or the call times out.
    pub async fn exec(&self, serial: &str, command: &str) -> Result<Vec<u8>, AdbError> {
        self.bounded(async {
            let service = format!("exec:{command}");
            let mut stream = self.open_device_service(serial, &service).await?;
            frame::read_to_end(&mut stream).await
        })
        .await
    }

    /// Starts `command` under the `shell,v2` protocol and streams its stdout. Only the setup is
    /// bounded.
    ///
    /// # Errors
    ///
    /// Returns an error when the phone isn't attached or the setup times out.
    pub async fn shell_stream(&self, serial: &str, command: &str) -> Result<ShellStream, AdbError> {
        self.bounded(async {
            let service = format!("shell,v2,raw:{command}");
            let stream = self.open_device_service(serial, &service).await?;
            Ok(ShellStream::new(stream))
        })
        .await
    }

    /// Opens a duplex stream service on the phone, such as `localabstract:scrcpy` or
    /// `tcp:27183`, and hands over the connection once the phone accepted it. Only the setup is
    /// bounded.
    ///
    /// # Errors
    ///
    /// Returns an error when the phone isn't attached, nothing listens there, or the setup
    /// times out.
    pub async fn open_stream(&self, serial: &str, service: &str) -> Result<TcpStream, AdbError> {
        let setup = async {
            let mut stream = self.connect().await?;
            frame::request(&mut stream, &format!("host:transport:{serial}")).await?;
            frame::request(&mut stream, service).await?;
            Ok(stream)
        };
        time::timeout(self.call_timeout, setup)
            .await
            .unwrap_or_else(|_| {
                Err(AdbError::new(
                    AdbErrorKind::Timeout,
                    format!("opening {service} on {serial} timed out"),
                ))
            })
    }

    /// Writes `data` to `remote_path` on the phone with the `sync:` protocol, as a regular file
    /// with permission bits `mode`, such as `0o644`, and modification time `modified`.
    ///
    /// # Errors
    ///
    /// Returns an error when the phone refuses the path, isn't attached, or the call times out.
    pub async fn push(
        &self,
        serial: &str,
        data: &[u8],
        remote_path: &str,
        mode: u32,
        modified: SystemTime,
    ) -> Result<(), AdbError> {
        if remote_path.contains('\0') {
            return Err(AdbError::new(
                AdbErrorKind::Protocol,
                "adb paths cannot contain NUL bytes".to_owned(),
            ));
        }
        self.bounded(async {
            let mut stream = self.open_device_service(serial, "sync:").await?;
            let file_mode = 0o100_000 | (mode & 0o7777);
            let target = format!("{remote_path},{file_mode}");
            frame::write_all(
                &mut stream,
                &frame::encode_sync_data(*b"SEND", target.as_bytes())?,
            )
            .await?;
            for chunk in data.chunks(frame::SYNC_DATA_MAX) {
                frame::write_all(&mut stream, &frame::encode_sync_data(*b"DATA", chunk)?).await?;
            }
            let mtime = modified.duration_since(UNIX_EPOCH).map_or(0, |since| {
                u32::try_from(since.as_secs()).unwrap_or(u32::MAX)
            });
            frame::write_all(&mut stream, &frame::encode_sync(*b"DONE", mtime, &[])).await?;
            frame::read_sync_status(&mut stream, &format!("push {remote_path}")).await?;
            frame::write_all(&mut stream, &frame::encode_sync(*b"QUIT", 0, &[])).await
        })
        .await
    }

    async fn connect(&self) -> Result<TcpStream, AdbError> {
        let address = (self.endpoint.host(), self.endpoint.port());
        let connecting = TcpStream::connect(address);
        let stream = time::timeout(CONNECT_TIMEOUT, connecting)
            .await
            .map_err(|_| {
                AdbError::new(
                    AdbErrorKind::Timeout,
                    format!(
                        "connecting to the adb server at {} timed out after {} ms",
                        self.endpoint,
                        CONNECT_TIMEOUT.as_millis()
                    ),
                )
            })?
            .map_err(|error| {
                AdbError::new(
                    AdbErrorKind::Connect,
                    format!("cannot reach the adb server at {}: {error}", self.endpoint),
                )
            })?;
        stream.set_nodelay(true).map_err(|error| {
            AdbError::new(
                AdbErrorKind::Connect,
                format!("cannot set up the connection to {}: {error}", self.endpoint),
            )
        })?;
        Ok(stream)
    }

    async fn open_device_service(
        &self,
        serial: &str,
        service: &str,
    ) -> Result<TcpStream, AdbError> {
        let mut stream = self.connect().await?;
        frame::request(&mut stream, &format!("host:transport:{serial}")).await?;
        frame::write_all(&mut stream, &frame::encode_request(service)?).await?;
        let name = service.split(':').next().unwrap_or(service);
        frame::read_status(&mut stream, &format!("{name} on {serial}")).await?;
        Ok(stream)
    }

    async fn bounded<T>(
        &self,
        call: impl Future<Output = Result<T, AdbError>>,
    ) -> Result<T, AdbError> {
        time::timeout(self.call_timeout, call)
            .await
            .unwrap_or_else(|_| {
                Err(AdbError::new(
                    AdbErrorKind::Timeout,
                    "the adb operation timed out".to_owned(),
                ))
            })
    }
}

/// A shell connection that closed before its exit packet, cleanly or mid-packet, means the phone
/// went away; other errors pass through.
fn ended_early(error: AdbError) -> AdbError {
    if error.kind() == AdbErrorKind::Closed {
        shell_ended_early(Some(error.message()))
    } else {
        error
    }
}

/// The error for a shell that ended without an exit code, with the connection's own reason when
/// there is one.
fn shell_ended_early(detail: Option<&str>) -> AdbError {
    let base = "the shell ended without reporting an exit code (device disconnected?)";
    let message = match detail {
        Some(detail) => format!("{base}: {detail}"),
        None => base.to_owned(),
    };
    AdbError::new(AdbErrorKind::Closed, message)
}
