//! The service's log: one JSON object per line on stderr, as spareline-old's `src/log.ts` wrote
//! it.

use std::io;
use std::sync::{Arc, Mutex};

use serde::Serialize;

use crate::clock::Clock;

/// Where the service logs: `{"at":<time>,"event":<name>,...fields}`, one object per line, with
/// the fields in the order they were added. The event names are spareline-old's (surface.md
/// section 11). No line holds a text's body, a phone number, an IMEI, an ICCID or a token.
///
/// Cloning is cheap; every clone writes to the same place.
#[derive(Clone, Debug)]
pub struct EventLog {
    clock: Clock,
    sink: Sink,
}

/// Where the lines go.
#[derive(Clone, Debug)]
enum Sink {
    Stderr,
    Discard,
    Memory(
        #[cfg_attr(
            dylint_lib = "trellis_lints",
            expect(
                shared_ownership,
                reason = "a test reads the lines that the tasks it starts write through their \
                          own clones of the log"
            )
        )]
        Arc<Mutex<Vec<String>>>,
    ),
}

impl EventLog {
    /// A log on stderr, its times read from `clock`.
    #[must_use]
    pub fn stderr(clock: Clock) -> Self {
        Self {
            clock,
            sink: Sink::Stderr,
        }
    }

    /// A log that writes nothing, for tests that don't read it.
    #[must_use]
    pub fn discard() -> Self {
        Self {
            clock: Clock::system(),
            sink: Sink::Discard,
        }
    }

    /// A log kept in memory, for tests that read it with [`EventLog::lines`].
    #[must_use]
    pub fn memory(clock: Clock) -> Self {
        Self {
            clock,
            sink: Sink::Memory(Arc::default()),
        }
    }

    /// Starts the line of `event`. Add its fields with [`LogLine::field`], then write it with
    /// [`LogLine::write`].
    pub fn event(&self, event: &str) -> LogLine<'_> {
        let mut text = String::from("{\"at\":");
        push_json(&mut text, &self.clock.now().to_string());
        text.push_str(",\"event\":");
        push_json(&mut text, event);
        LogLine { log: self, text }
    }

    /// Every line a memory log holds, oldest first; nothing for the others.
    ///
    /// # Panics
    ///
    /// Panics when a task panicked while writing to the same memory log, a bug.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        match &self.sink {
            Sink::Memory(lines) => lines
                .lock()
                .expect("no task panicked while writing a log line")
                .clone(),
            Sink::Stderr | Sink::Discard => Vec::new(),
        }
    }

    fn write(&self, line: String) {
        use std::io::Write as _;

        match &self.sink {
            Sink::Stderr => {
                let mut stderr = io::stderr().lock();
                match writeln!(stderr, "{line}") {
                    Ok(()) | Err(_) => {}
                }
            }
            Sink::Discard => {}
            Sink::Memory(lines) => lines
                .lock()
                .expect("no task panicked while writing a log line")
                .push(line),
        }
    }
}

/// One log line being built.
#[derive(Debug)]
#[must_use = "a log line is written only by `write`"]
pub struct LogLine<'log> {
    log: &'log EventLog,
    text: String,
}

impl LogLine<'_> {
    /// Adds the field `name` with `value` written as JSON. A value that can't be written as
    /// JSON, which only a map with keys other than strings is, is written as the error text.
    pub fn field<T: Serialize + ?Sized>(mut self, name: &str, value: &T) -> Self {
        self.text.push(',');
        push_json(&mut self.text, name);
        self.text.push(':');
        match serde_json::to_string(value) {
            Ok(json) => self.text.push_str(&json),
            Err(error) => push_json(&mut self.text, &error.to_string()),
        }
        self
    }

    /// Writes the line.
    pub fn write(mut self) {
        self.text.push('}');
        self.log.write(self.text);
    }
}

/// Appends `text` as a JSON string.
fn push_json(out: &mut String, text: &str) {
    use std::fmt::Write as _;

    match serde_json::to_string(text) {
        Ok(json) => out.push_str(&json),
        Err(_) => match write!(out, "{text:?}") {
            Ok(()) | Err(_) => {}
        },
    }
}

#[cfg(test)]
mod tests {
    use spareline_core::Timestamp;

    use super::*;

    #[test]
    fn writes_the_time_the_event_and_the_fields_in_order() {
        let log = EventLog::memory(Clock::fixed(Timestamp::from_millis(1_792_656_000_000)));
        log.event("phone-state")
            .field("serial", "SPARELINE00001")
            .field("state", "device")
            .field("seqs", &[9, 10])
            .write();
        assert_eq!(
            log.lines(),
            [
                r#"{"at":"2026-10-22T08:00:00.000Z","event":"phone-state","serial":"SPARELINE00001","state":"device","seqs":[9,10]}"#
            ]
        );
    }

    #[test]
    fn escapes_names_and_values() {
        let log = EventLog::memory(Clock::fixed(Timestamp::from_millis(0)));
        log.event("probe-failed")
            .field("error", "a \"quoted\"\nline")
            .write();
        assert_eq!(
            log.lines(),
            [
                r#"{"at":"1970-01-01T00:00:00.000Z","event":"probe-failed","error":"a \"quoted\"\nline"}"#
            ]
        );
    }

    #[test]
    fn a_discarding_log_keeps_nothing() {
        let log = EventLog::discard();
        log.event("listening").write();
        assert!(log.lines().is_empty(), "nothing is kept");
    }
}
