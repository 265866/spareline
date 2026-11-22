//! What happens to a phone's connection, for the tasks that follow phones.

/// A change in a phone's connection, published by [`Devices`](crate::Devices) to every
/// subscription.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum PhoneEvent {
    /// The phone came online (adb state `device`), and its probes start.
    Connected {
        /// The adb serial.
        serial: String,
    },
    /// The phone's first identity read of this connection is done, applied or given up after
    /// its retries, so its SIM placements are as current as they will get. Tasks that map sub
    /// ids to SIMs, such as text ingest, start here rather than at `Connected`.
    Ready {
        /// The adb serial.
        serial: String,
    },
    /// The phone went offline or out of adb's list; its calls were cancelled.
    Disconnected {
        /// The adb serial.
        serial: String,
    },
    /// An identity read changed which SIM is in which slot, or a SIM's number.
    SubsChanged {
        /// The adb serial.
        serial: String,
    },
}

impl PhoneEvent {
    /// The serial of the phone the event is about.
    #[must_use]
    pub fn serial(&self) -> &str {
        match self {
            Self::Connected { serial }
            | Self::Ready { serial }
            | Self::Disconnected { serial }
            | Self::SubsChanged { serial } => serial,
        }
    }
}
