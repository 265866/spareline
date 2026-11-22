//! The phones' live screens: one scrcpy-server session per phone, shared by every browser that
//! watches it, with remote control through the session's control socket.

mod frame_rate;
mod phone_screen;
mod scrcpy_session;
mod screen_command;
mod screen_hub;
mod screen_message;
mod screen_session;
mod screen_timings;
mod screens;
mod task_guard;
mod viewer;
mod viewer_close;
mod viewer_frame;
mod viewer_queue;

pub use frame_rate::FrameRate;
pub use screen_session::{ScreenSession, ScreenState};
pub use screens::{SCREEN_SOCKET_LIMIT, Screens};

#[cfg(test)]
mod fake_scrcpy;
#[cfg(test)]
mod relay_tests;
#[cfg(test)]
mod screen_harness;
#[cfg(test)]
mod socket_tests;
