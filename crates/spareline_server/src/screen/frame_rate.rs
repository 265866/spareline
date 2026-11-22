//! A session's video frame rate, as the API reports it.

use serde::{Serialize, Serializer};

/// Frames per second rounded to one decimal, as spareline-old's `Math.round(x * 10) / 10` gave
/// it. It serializes as JavaScript printed the number: `30` for a whole rate, `12.5` otherwise.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct FrameRate {
    tenths: u64,
}

impl FrameRate {
    /// The rate of `tenths` tenths of a frame per second.
    #[must_use]
    pub const fn from_tenths(tenths: u64) -> Self {
        Self { tenths }
    }

    /// The rate in tenths of a frame per second.
    #[must_use]
    pub const fn tenths(self) -> u64 {
        self.tenths
    }
}

impl Serialize for FrameRate {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if self.tenths.is_multiple_of(10) {
            serializer.serialize_u64(self.tenths / 10)
        } else {
            #[expect(
                clippy::cast_precision_loss,
                reason = "a frame rate in tenths stays far below 2^52, where f64 is exact"
            )]
            let rate = self.tenths as f64 / 10.0;
            serializer.serialize_f64(rate)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_rates_print_without_a_decimal_point_as_javascript_did() {
        let printed = serde_json::to_string(&[
            FrameRate::from_tenths(0),
            FrameRate::from_tenths(300),
            FrameRate::from_tenths(125),
            FrameRate::from_tenths(1),
        ])
        .unwrap();
        assert_eq!(printed, "[0,30,12.5,0.1]");
    }
}
