//! Which streams an event-stream client follows, and where it is in each.

use std::collections::BTreeSet;

use crate::api_error::ApiError;

/// The streams `GET /api/v1/events` serves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamKinds {
    /// Device events, `event: device`.
    pub device: bool,
    /// Texts, `event: message`.
    pub message: bool,
}

impl StreamKinds {
    /// Reads the `types` query parameters, each a comma-separated list of `device` and
    /// `message`. None, or only empty values, means `device`.
    ///
    /// # Errors
    ///
    /// Returns a 400 for any other token, an empty one from a stray comma included, naming the
    /// raw values as spareline-old did.
    pub fn parse(values: &[String]) -> Result<Self, ApiError> {
        if values.iter().all(String::is_empty) {
            return Ok(Self {
                device: true,
                message: false,
            });
        }
        let tokens: BTreeSet<&str> = values
            .iter()
            .flat_map(|value| value.split(','))
            .map(str::trim)
            .collect();
        let device = tokens.contains("device");
        let message = tokens.contains("message");
        if usize::from(device) + usize::from(message) != tokens.len() {
            return Err(ApiError::bad_request(format!(
                "types must be device, message, or both, not \"{}\"",
                values.join(",")
            )));
        }
        Ok(Self { device, message })
    }
}

/// Where a client is in each stream: the last device event id and the last text `seq` it saw,
/// or `None` for a stream it has no cursor for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StreamCursor {
    /// The last device event id seen.
    pub device: Option<i64>,
    /// The last text `seq` seen.
    pub message: Option<i64>,
}

impl StreamCursor {
    /// Reads a cursor in the form the stream writes for the same `kinds`: `<id>`, `<seq>`, or
    /// `<id>:<seq>` with both. A bare number with both kinds is a device-event id. Anything else
    /// means no cursor.
    #[must_use]
    pub fn parse(raw: Option<&str>, kinds: StreamKinds) -> Self {
        let value = raw.unwrap_or("").trim();
        let both = kinds.device && kinds.message;
        if both
            && let Some((device, message)) = value.split_once(':')
            && let (Some(device), Some(message)) = (number(device), number(message))
        {
            return Self {
                device: Some(device),
                message: Some(message),
            };
        }
        let Some(single) = number(value) else {
            return Self::default();
        };
        if both || kinds.device {
            Self {
                device: Some(single),
                message: None,
            }
        } else {
            Self {
                device: None,
                message: Some(single),
            }
        }
    }
}

/// The `id:` the stream writes for its position: the device id, the seq, or both joined by `:`.
#[must_use]
pub fn event_id(kinds: StreamKinds, device: i64, message: i64) -> String {
    match (kinds.device, kinds.message) {
        (true, true) => format!("{device}:{message}"),
        (false, true) => message.to_string(),
        (true | false, false) => device.to_string(),
    }
}

/// A run of ASCII digits as a number, or `None`.
fn number(text: &str) -> Option<i64> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEVICE: StreamKinds = StreamKinds {
        device: true,
        message: false,
    };
    const MESSAGE: StreamKinds = StreamKinds {
        device: false,
        message: true,
    };
    const BOTH: StreamKinds = StreamKinds {
        device: true,
        message: true,
    };

    fn kinds(values: &[&str]) -> Result<StreamKinds, ApiError> {
        let owned: Vec<String> = values.iter().map(|value| (*value).to_owned()).collect();
        StreamKinds::parse(&owned)
    }

    #[test]
    fn reads_the_types_parameter() {
        assert_eq!(kinds(&[]).unwrap(), DEVICE);
        assert_eq!(kinds(&[""]).unwrap(), DEVICE);
        assert_eq!(kinds(&["message"]).unwrap(), MESSAGE);
        assert_eq!(kinds(&["device,message"]).unwrap(), BOTH);
        assert_eq!(kinds(&["message", " device "]).unwrap(), BOTH);
        assert_eq!(kinds(&["device,device"]).unwrap(), DEVICE);
    }

    #[test]
    fn refuses_unknown_and_empty_tokens() {
        let error = kinds(&["foo"]).unwrap_err();
        assert_eq!(
            error.message(),
            "types must be device, message, or both, not \"foo\""
        );
        let error = kinds(&["device,"]).unwrap_err();
        assert_eq!(
            error.message(),
            "types must be device, message, or both, not \"device,\""
        );
    }

    #[test]
    fn reads_the_cursor_in_the_form_the_stream_writes() {
        assert_eq!(
            StreamCursor::parse(Some("7:42"), BOTH),
            StreamCursor {
                device: Some(7),
                message: Some(42)
            }
        );
        assert_eq!(StreamCursor::parse(Some("7"), BOTH).device, Some(7));
        assert_eq!(StreamCursor::parse(Some("7"), BOTH).message, None);
        assert_eq!(StreamCursor::parse(Some("42"), MESSAGE).message, Some(42));
        assert_eq!(StreamCursor::parse(Some(" 9 "), DEVICE).device, Some(9));
        for raw in [
            None,
            Some(""),
            Some("x"),
            Some("7:42"),
            Some("-1"),
            Some("1:2:3"),
        ] {
            assert_eq!(
                StreamCursor::parse(raw, DEVICE),
                StreamCursor::default(),
                "{raw:?}"
            );
        }
    }

    #[test]
    fn writes_ids_for_the_streams_followed() {
        assert_eq!(event_id(DEVICE, 7, 42), "7");
        assert_eq!(event_id(MESSAGE, 7, 42), "42");
        assert_eq!(event_id(BOTH, 7, 42), "7:42");
    }
}
