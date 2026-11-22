//! The phone manager: the one owner of device state. It follows the adb server's device list,
//! probes each phone's identity and health, records phones, SIMs and numbers in the database,
//! and is the only code that opens calls to phones, through [`Devices`].

mod adb_status;
mod device_error;
mod device_manager;
mod device_options;
#[cfg_attr(
    dylint_lib = "trellis_lints",
    expect(
        file_named_after_item,
        reason = "Devices can't live in devices/devices.rs, which clippy's module_inception \
                  rejects, so its file is named for what it is, the handle"
    )
)]
mod handle;
mod live_phone;
mod manager_context;
mod phone_event;
mod phone_shell;
mod phone_stream;
mod phone_table;
mod probe_reason;
mod probe_worker;
mod session;

pub use adb_status::AdbStatus;
pub use device_error::DeviceError;
pub use device_manager::DeviceManager;
pub use device_options::{DeviceOptions, MAX_BACKOFF};
pub use handle::{Devices, PUSH_TIMEOUT};
pub use live_phone::LivePhone;
pub use phone_event::PhoneEvent;
pub use phone_shell::PhoneShell;
pub use phone_stream::PhoneStream;
pub(crate) use session::SessionToken;
