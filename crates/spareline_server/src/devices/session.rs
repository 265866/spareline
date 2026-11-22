//! A phone's connection session: what cancels its calls when it goes offline.

use std::future::Future;
use std::pin;

use futures_util::future::{self, Either};

use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::watch::{self, Receiver, Sender};

use crate::devices::device_error::DeviceError;
use crate::devices::probe_reason::ProbeReason;

/// The session of a phone that is online, from the moment adb reports it as `device` until it
/// reports anything else. Dropping it ends the session: every call made through one of its
/// tokens is cancelled, as spareline-old's session `AbortController` cancelled them, and its
/// probe worker stops after the probe it is running.
#[derive(Debug)]
pub(crate) struct Session {
    id: u64,
    ended: Sender<()>,
    probes: UnboundedSender<ProbeReason>,
}

/// A handle on a [`Session`] that tells whether the session ended.
#[derive(Clone, Debug)]
pub(crate) struct SessionToken {
    id: u64,
    ended: Receiver<()>,
}

impl Session {
    /// A new session numbered `id`, whose probes go to `probes`.
    pub(crate) fn new(id: u64, probes: UnboundedSender<ProbeReason>) -> Self {
        Self {
            id,
            ended: watch::channel(()).0,
            probes,
        }
    }

    /// The session's number, unique in the run.
    pub(crate) fn id(&self) -> u64 {
        self.id
    }

    /// A token that follows this session.
    pub(crate) fn token(&self) -> SessionToken {
        SessionToken {
            id: self.id,
            ended: self.ended.subscribe(),
        }
    }

    /// Hands a probe to the session's worker. A worker that already stopped takes none, which
    /// only happens while the session is ending.
    pub(crate) fn send_probe(&self, reason: ProbeReason) -> bool {
        self.probes.send(reason).is_ok()
    }
}

impl SessionToken {
    /// The number of the session the token follows.
    pub(crate) fn id(&self) -> u64 {
        self.id
    }

    /// Whether the session ended.
    pub(crate) fn is_ended(&self) -> bool {
        self.ended.has_changed().is_err()
    }

    /// Finishes once the session ended.
    pub(crate) async fn ended(&self) {
        let mut ended = self.ended.clone();
        while ended.changed().await.is_ok() {}
    }

    /// Runs `call`, cancelling it when the session ends first.
    ///
    /// # Errors
    ///
    /// [`DeviceError::Disconnected`] when the session ended before `call` finished.
    pub(crate) async fn guard<F: Future>(
        &self,
        serial: &str,
        call: F,
    ) -> Result<F::Output, DeviceError> {
        let call = pin::pin!(call);
        let ended = pin::pin!(self.ended());
        match future::select(call, ended).await {
            Either::Left((output, _)) => Ok(output),
            Either::Right(((), _)) => Err(DeviceError::Disconnected {
                serial: serial.to_owned(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::future;

    use tokio::runtime::Builder;
    use tokio::sync::mpsc;

    use super::*;

    fn block_on<F: Future>(future: F) -> F::Output {
        Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(future)
    }

    #[test]
    fn a_token_sees_its_session_end_when_the_session_is_dropped() {
        let session = Session::new(7, mpsc::unbounded_channel().0);
        let token = session.token();
        assert_eq!(token.id(), 7);
        assert!(!token.is_ended(), "the session is live");
        drop(session);
        assert!(token.is_ended(), "dropping the session ends it");
        block_on(token.ended());
    }

    #[test]
    fn a_guarded_call_is_cancelled_when_the_session_ends() {
        let session = Session::new(1, mpsc::unbounded_channel().0);
        let token = session.token();
        assert_eq!(block_on(token.guard("X1", future::ready(5))), Ok(5));
        drop(session);
        assert_eq!(
            block_on(token.guard("X1", future::pending::<()>())),
            Err(DeviceError::Disconnected {
                serial: "X1".to_owned()
            })
        );
    }
}
