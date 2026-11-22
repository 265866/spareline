//! A client for the adb server's smart-socket protocol, the one the `adb` command talks to.
//!
//! Every call opens its own TCP connection to the adb server, as the `adb` command does, so calls
//! are independent and a failed call can't poison another. Nothing here talks to a phone
//! directly: the adb server owns USB and forwards each request.

mod adb_client;
mod adb_error;
mod device_entry;
mod device_tracker;
mod endpoint;
mod frame;
mod frame_reader;
mod shell_header;
mod shell_output;
mod shell_stream;

pub use adb_client::AdbClient;
pub use adb_error::{AdbError, AdbErrorKind};
pub use device_entry::{DeviceEntry, DeviceState};
pub use device_tracker::DeviceTracker;
pub use endpoint::{Endpoint, EndpointError};
pub use shell_output::ShellOutput;
pub use shell_stream::ShellStream;
