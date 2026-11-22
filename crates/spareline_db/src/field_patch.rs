//! One field of a partial update.

use serde::{Serialize, Serializer};

/// One field of a `PATCH` body: left out, set to `null`, or set to a value. It is written to
/// `inventory_edits.changes_json` as the request had it: left out entirely, `null`, or the
/// value.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum FieldPatch<T> {
    /// The field wasn't in the request, so it stays as it is.
    #[default]
    Absent,
    /// The field was `null`, so it is cleared.
    Null,
    /// The field was set.
    Value(T),
}

impl<T> FieldPatch<T> {
    /// Whether the field wasn't in the request.
    #[must_use]
    pub fn is_absent(&self) -> bool {
        matches!(self, Self::Absent)
    }

    /// The new value, `None` to clear, or `None` from [`FieldPatch::Absent`] too: check
    /// [`FieldPatch::is_absent`] first.
    pub(crate) fn as_option(&self) -> Option<&T> {
        match self {
            Self::Value(value) => Some(value),
            Self::Absent | Self::Null => None,
        }
    }
}

impl<T: Serialize> Serialize for FieldPatch<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Value(value) => value.serialize(serializer),
            Self::Absent | Self::Null => serializer.serialize_none(),
        }
    }
}
