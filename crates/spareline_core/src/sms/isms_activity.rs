//! What `dumpsys isms` shows of inbound SMS handling.

use crate::text;

/// The trigger command, every 3 s per online phone. It keeps only the lines [`parse_isms`]
/// reads; full `dumpsys isms` is about 15 KB.
pub const ISMS_TRIGGER_COMMAND: &str = "dumpsys isms | grep -E 'InboundSmsHandler:$|total records=|what=EVENT_(NEW|BROADCAST)_SMS|Delivering SMS to'";

/// What `dumpsys isms` shows of inbound SMS handling. Any difference between two reads means the
/// phone handled an inbound SMS, or a WAP push for an MMS, since the first one, so the providers
/// are worth querying.
///
/// spareline-old compared the JSON of these four fields; comparing two activities with `==` is
/// the same test.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct IsmsActivity {
    /// The `total records=N` of each `InboundSmsHandler` state machine, in order. It grows with
    /// every message the handler processes.
    pub handler_records: Vec<u64>,
    /// The newest `EVENT_NEW_SMS` or `EVENT_BROADCAST_SMS` record time as printed, such as
    /// `10-21 19:50:27.212`: no year, phone clock. The newest is the greatest string.
    pub last_event_at: Option<String>,
    /// The number of `Delivering SMS to: ...` lines in the dispatcher log.
    pub deliveries: u64,
    /// The time of the newest of those lines as printed, the greatest string.
    pub last_delivery_at: Option<String>,
}

/// Reads the output of [`ISMS_TRIGGER_COMMAND`]. Lines it doesn't recognize are skipped. A
/// `total records=N` line counts only right after an `InboundSmsHandler:` line.
#[must_use]
pub fn parse_isms(output: &str) -> IsmsActivity {
    let mut activity = IsmsActivity::default();
    let mut in_handler = false;
    for line in output.replace("\r\n", "\n").split('\n') {
        let trimmed = text::trim(line);
        if trimmed.ends_with("InboundSmsHandler:") {
            in_handler = true;
            continue;
        }
        if let Some(records) = total_records(trimmed) {
            if in_handler {
                activity.handler_records.push(records);
            }
            in_handler = false;
            continue;
        }
        if let Some(time) = event_time(trimmed) {
            if activity
                .last_event_at
                .as_deref()
                .is_none_or(|last| time > last)
            {
                activity.last_event_at = Some(time.to_owned());
            }
            continue;
        }
        if let Some(time) = delivery_time(trimmed) {
            activity.deliveries += 1;
            if activity
                .last_delivery_at
                .as_deref()
                .is_none_or(|last| time > last)
            {
                activity.last_delivery_at = Some(time.to_owned());
            }
        }
    }
    activity
}

/// `/^total records=(\d+)$/`. A count too large for `u64` reads as `u64::MAX`.
fn total_records(line: &str) -> Option<u64> {
    let digits = line.strip_prefix("total records=")?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some(digits.parse().unwrap_or(u64::MAX))
}

/// The time in `/^rec\[\d+\]: time=(\S+ \S+) .*what=EVENT_(?:NEW|BROADCAST)_SMS\b/`.
fn event_time(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("rec[")?;
    let digits = text::leading_digits(rest);
    if digits.is_empty() {
        return None;
    }
    let rest = rest[digits.len()..].strip_prefix("]: time=")?;
    let date = text::leading_non_space(rest);
    let clock = text::leading_non_space(rest[date.len()..].strip_prefix(' ')?);
    if date.is_empty() || clock.is_empty() {
        return None;
    }
    let time_length = date.len() + 1 + clock.len();
    let after = rest[time_length..].strip_prefix(' ')?;
    let searched = &after[..after.find(text::is_line_terminator).unwrap_or(after.len())];
    let event = ["what=EVENT_NEW_SMS", "what=EVENT_BROADCAST_SMS"]
        .iter()
        .any(|event| {
            text::occurrences(searched, event)
                .any(|index| text::word_boundary_after(searched, index + event.len()))
        });
    event.then(|| &rest[..time_length])
}

/// The time in `/^(\S+) - Delivering SMS to: /`.
fn delivery_time(line: &str) -> Option<&str> {
    let time = text::leading_non_space(line);
    if time.is_empty() || !line[time.len()..].starts_with(" - Delivering SMS to: ") {
        return None;
    }
    Some(time)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_inbound_handler_output_at_all() {
        assert_eq!(parse_isms(""), IsmsActivity::default());
    }

    #[test]
    fn records_count_only_after_a_handler_line() {
        let activity = parse_isms(
            "total records=9\n  GsmInboundSmsHandler:\n  total records=5\ntotal records=7\n",
        );
        assert_eq!(activity.handler_records, [5]);
    }

    #[test]
    fn event_lines() {
        let line = "rec[3]: time=10-21 19:50:27.212 processed=A what=EVENT_NEW_SMS";
        assert_eq!(event_time(line), Some("10-21 19:50:27.212"));
        assert_eq!(event_time(&format!("{line}X")), None);
        assert_eq!(event_time(&format!("{line}.")), Some("10-21 19:50:27.212"));
        assert_eq!(
            event_time("rec[3]: time=a b what=EVENT_BROADCAST_SMS"),
            Some("a b")
        );
        assert_eq!(event_time("rec[3]: time=a b\rwhat=EVENT_NEW_SMS"), None);
        assert_eq!(event_time("rec[]: time=a b what=EVENT_NEW_SMS"), None);
        assert_eq!(event_time("rec[3]: time=a  b what=EVENT_NEW_SMS"), None);
        assert_eq!(event_time("rec[3]: time=a b what=EVENT_OTHER"), None);
    }

    #[test]
    fn the_newest_event_and_delivery_win() {
        let activity = parse_isms(
            "rec[1]: time=10-21 19:50:27.278 x what=EVENT_NEW_SMS\n\
             rec[2]: time=10-21 09:00:00.000 x what=EVENT_NEW_SMS\n\
             2026-10-21T19:50:27.267498 - Delivering SMS to: a\n\
             2026-10-21T09:00:00.000000 - Delivering SMS to: b\n",
        );
        assert_eq!(
            activity.last_event_at.as_deref(),
            Some("10-21 19:50:27.278")
        );
        assert_eq!(activity.deliveries, 2);
        assert_eq!(
            activity.last_delivery_at.as_deref(),
            Some("2026-10-21T19:50:27.267498")
        );
    }
}
