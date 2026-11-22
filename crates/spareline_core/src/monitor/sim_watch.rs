//! The SIM watch: the radio log stream and the SIM state poll that signal a possible SIM change,
//! and what changed between two reads of the slots.

use std::collections::BTreeMap;

use crate::android::SubscriptionRecord;
use crate::text;

/// The radio log lines that mean a SIM card changed state, as a `logcat -e` pattern.
///
/// On the B30's user build the radio buffer carries the RIL's own lines (requests as
/// `[1234]> NAME`, unsolicited ones as `[UNSL]< NAME` or `[UNSL]< MTK: RIL_UNSOL_...`, and the
/// modem's AT traffic), not the Java `UiccController` logs. These names come from AOSP's RIL and
/// MTK's (`SIM_PLUG_IN`/`OUT`, `SIM_MISSING`, `SIM_RECOVERY`, `COMMON_SLOT`, and the `+ESIMS`
/// hot-plug URC). A swap can't be produced on demand, so they are unconfirmed on the B30; the
/// SIM state poll is the backstop, and a false hit only costs one extra `dumpsys isub`.
pub const SIM_LOG_PATTERN: &str =
    "SIM_STATUS|SIM_PLUG|SIM_MISSING|SIM_RECOVERY|SIM_COMMON_SLOT|SLOT_STATUS|ICC_STATUS|[+]ESIMS";

/// The radio log stream, run over the shell protocol: [`SIM_LOG_PATTERN`] from the newest line on
/// (`-T 1`), rather than a replay of the buffer.
pub const SIM_LOG_COMMAND: &str = "logcat -b radio -v brief -T 1 -e 'SIM_STATUS|SIM_PLUG|SIM_MISSING|SIM_RECOVERY|SIM_COMMON_SLOT|SLOT_STATUS|ICC_STATUS|[+]ESIMS'";

/// The SIM state poll, every 10 s.
pub const SIM_STATE_COMMAND: &str = "getprop gsm.sim.state";

/// What signalled a possible SIM change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SimSignal {
    /// A radio log line.
    Logcat,
    /// A change in `gsm.sim.state`.
    SimState,
}

impl SimSignal {
    /// The signal's name as the log spells it: `logcat` or `sim_state`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Logcat => "logcat",
            Self::SimState => "sim_state",
        }
    }
}

/// Finds the radio log lines that signal a SIM change in the bytes of the stream, as they
/// arrive. A partial last line waits for the rest.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SimLogScanner {
    pending: Vec<u8>,
}

impl SimLogScanner {
    /// Takes the next bytes of the stream, and says whether a complete line among them signals a
    /// SIM change: any line with text in it other than logcat's `-----` buffer headers. The line
    /// itself isn't kept or returned, since it can carry identifiers.
    pub fn push(&mut self, bytes: &[u8]) -> bool {
        self.pending.extend_from_slice(bytes);
        let Some(end) = self.pending.iter().rposition(|&byte| byte == b'\n') else {
            return false;
        };
        let complete: Vec<u8> = self.pending.drain(..=end).collect();
        complete[..end].split(|&byte| byte == b'\n').any(|line| {
            let line = String::from_utf8_lossy(line);
            !text::trim(&line).is_empty() && !line.starts_with("-----")
        })
    }
}

/// Tracks `gsm.sim.state` between polls.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SimStatePoll {
    last: Option<String>,
}

impl SimStatePoll {
    /// Takes the output of [`SIM_STATE_COMMAND`], and says whether the state changed since the
    /// last poll. The first poll only records the state. A poll that failed isn't passed here, so
    /// it leaves the last state as it was.
    pub fn observe(&mut self, stdout: &str) -> bool {
        let state = text::trim(stdout);
        let changed = self.last.as_deref().is_some_and(|last| last != state);
        self.last = Some(state.to_owned());
        changed
    }
}

/// How a slot's SIM changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SimChangeKind {
    /// A SIM is in a slot that had none.
    Inserted,
    /// A slot's SIM is gone.
    Removed,
    /// A slot holds a different SIM.
    Swapped,
}

impl SimChangeKind {
    /// The kind's name: `inserted`, `removed` or `swapped`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inserted => "inserted",
            Self::Removed => "removed",
            Self::Swapped => "swapped",
        }
    }
}

/// One slot's change between two reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SimChange {
    /// The slot.
    pub slot: u32,
    /// How it changed.
    pub kind: SimChangeKind,
    /// The sub id before, if any.
    pub from: Option<i64>,
    /// The sub id after, if any.
    pub to: Option<i64>,
}

/// The sub id in each slot, from `dumpsys isub`'s active subscriptions. A slot listed twice
/// keeps its last sub id.
#[must_use]
pub fn slot_map(active: &[SubscriptionRecord]) -> BTreeMap<u32, i64> {
    active.iter().map(|sub| (sub.slot, sub.sub_id)).collect()
}

/// What changed between two slot maps, by slot. Android gives each ICCID its own subscription id
/// and reuses it when the same card returns, so a slot whose sub id changes holds a different
/// card, and the same card moved to another slot is a removal and an insertion.
#[must_use]
pub fn diff_slots(before: &BTreeMap<u32, i64>, after: &BTreeMap<u32, i64>) -> Vec<SimChange> {
    let mut slots: Vec<u32> = before.keys().chain(after.keys()).copied().collect();
    slots.sort_unstable();
    slots.dedup();
    slots
        .into_iter()
        .filter_map(|slot| {
            let from = before.get(&slot).copied();
            let to = after.get(&slot).copied();
            let kind = match (from, to) {
                (None, Some(_)) => SimChangeKind::Inserted,
                (Some(_), None) => SimChangeKind::Removed,
                (Some(old), Some(new)) if old != new => SimChangeKind::Swapped,
                _ => return None,
            };
            Some(SimChange {
                slot,
                kind,
                from,
                to,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(entries: &[[i64; 2]]) -> BTreeMap<u32, i64> {
        entries
            .iter()
            .map(|&[slot, sub]| (u32::try_from(slot).expect("a slot"), sub))
            .collect()
    }

    fn change(slot: u32, kind: SimChangeKind, from: Option<i64>, to: Option<i64>) -> SimChange {
        SimChange {
            slot,
            kind,
            from,
            to,
        }
    }

    #[test]
    fn slot_diffs() {
        assert_eq!(diff_slots(&map(&[[0, 1]]), &map(&[[0, 1]])), []);
        assert_eq!(
            diff_slots(&map(&[[0, 1]]), &map(&[[0, 1], [1, 2]])),
            [change(1, SimChangeKind::Inserted, None, Some(2))]
        );
        assert_eq!(
            diff_slots(&map(&[[0, 1], [1, 2]]), &map(&[[1, 2]])),
            [change(0, SimChangeKind::Removed, Some(1), None)]
        );
        assert_eq!(
            diff_slots(&map(&[[0, 1]]), &map(&[[0, 3]])),
            [change(0, SimChangeKind::Swapped, Some(1), Some(3))]
        );
        assert_eq!(
            diff_slots(&map(&[[0, 1]]), &map(&[[1, 1]])),
            [
                change(0, SimChangeKind::Removed, Some(1), None),
                change(1, SimChangeKind::Inserted, None, Some(1)),
            ],
            "the same card in another slot keeps its sub id"
        );
    }

    #[test]
    fn log_lines_signal_once_complete() {
        let mut scanner = SimLogScanner::default();
        assert!(
            !scanner.push(b"--------- beginning of radio\n"),
            "a buffer header"
        );
        assert!(
            !scanner.push(b"D/RILJ ( 1): [UNSL]< UNSOL_RESPONSE_SIM_"),
            "a partial line"
        );
        assert!(scanner.push(b"STATUS_CHANGED\n"), "the line completes");
        assert!(!scanner.push(b"  \r\n\n"), "blank lines");
        assert!(!scanner.push(&[0xe2, 0x9c]), "half a character");
        assert!(
            scanner.push(&[0x93, b'\n']),
            "the character completes a line"
        );
    }

    #[test]
    fn sim_state_changes_after_the_first_poll() {
        let mut poll = SimStatePoll::default();
        assert!(
            !poll.observe("LOADED,ABSENT\n"),
            "the first poll only records"
        );
        assert!(!poll.observe("LOADED,ABSENT"), "unchanged");
        assert!(poll.observe("ABSENT,ABSENT\n"), "changed");
        assert!(!poll.observe(" ABSENT,ABSENT "), "unchanged once trimmed");
    }

    #[test]
    fn names() {
        assert_eq!(SimSignal::Logcat.as_str(), "logcat");
        assert_eq!(SimSignal::SimState.as_str(), "sim_state");
        assert_eq!(SimChangeKind::Swapped.as_str(), "swapped");
        assert_eq!(
            SIM_LOG_COMMAND,
            format!("logcat -b radio -v brief -T 1 -e '{SIM_LOG_PATTERN}'")
        );
    }
}
