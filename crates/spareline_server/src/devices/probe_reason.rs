//! Why a phone is probed.

/// Why a probe of a phone runs, which decides what it reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ProbeReason {
    /// The phone came online: identity with its retries, then `Ready`, then health.
    Connect,
    /// The periodic round: health, then identity when it is due.
    Health,
    /// A refresh, such as after a SIM change: identity only.
    Identity,
}

impl ProbeReason {
    /// The reason as spareline-old's `probe-failed` log line named it.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Connect => "connect",
            Self::Health => "health",
            Self::Identity => "identity",
        }
    }
}
