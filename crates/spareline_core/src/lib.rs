//! The logic behind `spareline`, apart from its binary so the binary's command-line and
//! runtime dependencies stay out of it.

mod timestamp;
mod version_line;

pub use timestamp::Timestamp;
pub use version_line::version_line;
