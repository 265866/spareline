//! The settings tables `settings list` reads.

/// One of Android's settings tables.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Namespace {
    /// `global`.
    Global,
    /// `secure`.
    Secure,
    /// `system`.
    System,
}

impl Namespace {
    /// Every table, in the order the read lists them.
    pub const ALL: [Self; 3] = [Self::Global, Self::Secure, Self::System];

    /// The table's name on the `settings` command line.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Secure => "secure",
            Self::System => "system",
        }
    }
}
