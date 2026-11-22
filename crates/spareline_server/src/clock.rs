//! Where the service reads the current time, so tests can set it.

use std::fmt::{self, Debug, Formatter};
use std::sync::Arc;

use spareline_core::Timestamp;
use tokio::time::Instant;

/// The function a [`Clock`] reads the time from.
type Source = dyn Fn() -> Timestamp + Send + Sync;

/// The current time as the service writes it. The service runs on [`Clock::system`]; a test
/// passes a clock it controls, such as [`Clock::fixed`] or [`Clock::tokio`], whose time moves
/// with tokio's paused clock.
///
/// Cloning is cheap and every clone reads the same source.
#[derive(Clone)]
pub struct Clock {
    #[cfg_attr(
        dylint_lib = "trellis_lints",
        expect(
            shared_ownership,
            reason = "every task of the service reads the one clock, which a test may swap for \
                      a closure that captures its own state"
        )
    )]
    source: Arc<Source>,
}

impl Clock {
    /// The system clock.
    #[must_use]
    pub fn system() -> Self {
        Self::from_fn(Timestamp::now)
    }

    /// A clock that always reads `at`.
    #[must_use]
    pub fn fixed(at: Timestamp) -> Self {
        Self::from_fn(move || at)
    }

    /// A clock that reads `start` now, and moves on with tokio's clock, so a test on a paused
    /// runtime advances both with `tokio::time::advance`. Call it inside the runtime.
    #[must_use]
    pub fn tokio(start: Timestamp) -> Self {
        let origin = Instant::now();
        Self::from_fn(move || start.plus(origin.elapsed()))
    }

    /// A clock that reads `source`.
    #[must_use]
    pub fn from_fn<F: Fn() -> Timestamp + Send + Sync + 'static>(source: F) -> Self {
        Self {
            source: Arc::new(source),
        }
    }

    /// The current time.
    #[must_use]
    pub fn now(&self) -> Timestamp {
        (self.source)()
    }
}

impl Debug for Clock {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("Clock").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::runtime::Builder;
    use tokio::time;

    use super::*;

    const START: Timestamp = Timestamp::from_millis(1_793_000_000_000);

    #[test]
    fn a_fixed_clock_stands_still() {
        let clock = Clock::fixed(START);
        assert_eq!(clock.now(), START);
        assert_eq!(clock.clone().now(), START);
    }

    #[test]
    fn a_tokio_clock_moves_with_the_paused_runtime() {
        Builder::new_current_thread()
            .enable_all()
            .start_paused(true)
            .build()
            .unwrap()
            .block_on(async {
                let clock = Clock::tokio(START);
                assert_eq!(clock.now(), START);
                time::advance(Duration::from_secs(90)).await;
                assert_eq!(clock.now(), START.plus(Duration::from_secs(90)));
            });
    }

    #[test]
    fn the_system_clock_reads_the_current_time() {
        let before = Timestamp::now();
        let read = Clock::system().now();
        assert!(read >= before, "{read} is not before {before}");
    }
}
