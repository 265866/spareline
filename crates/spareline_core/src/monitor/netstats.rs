//! `dumpsys netstats --full --uid` on Android 14.

use crate::text::{self, Sign, Start};

/// Which history of `dumpsys netstats` an entry is from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NetSection {
    /// `Xt stats:`, per network.
    Xt,
    /// `UID stats:`, per network, uid, counter set and tag.
    Uid,
}

/// One network history entry: an `ident=[...] uid=N set=S tag=T` line and its buckets.
///
/// ```text
/// UID stats:
///   ident=[{type=0, ratType=13, subscriberId=310240..., metered=false, ..., subId=1}] uid=0 set=DEFAULT tag=0x0
///     NetworkStatsHistory: bucketDuration=7200
///       st=1792627200 rb=0 rp=0 tb=48 tp=1 op=0
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetEntry {
    /// The history it is from.
    pub section: NetSection,
    /// A cellular network: an identity with `type=0` (`NetworkTemplate`'s mobile type).
    pub mobile: bool,
    /// An identity with `metered=true`.
    pub metered: bool,
    /// The first identity's `subId` that has one, when it is an integer.
    pub sub_id: Option<i64>,
    /// The uid; -1 in the per-network history.
    pub uid: i64,
    /// The tag as printed, such as `0x0`.
    pub tag: String,
    /// `bucketDuration` in seconds; 0 until the history names it.
    pub bucket_seconds: i64,
    /// The buckets, in order.
    pub buckets: Vec<NetBucket>,
}

/// One bucket of a history.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NetBucket {
    /// `st`: the bucket's start, in seconds by the phone's clock.
    pub start: i64,
    /// `rb`: bytes received.
    pub rx: u64,
    /// `tb`: bytes sent.
    pub tx: u64,
}

/// Reads the `Xt stats:` and `UID stats:` histories of `dumpsys netstats --full --uid`. A line
/// that starts without white space starts a section, and every other section is skipped; lines
/// it doesn't recognize are skipped too. A number too large for its type reads as that type's
/// largest value.
#[must_use]
pub fn parse_netstats(output: &str) -> Vec<NetEntry> {
    let mut entries: Vec<NetEntry> = Vec::new();
    let mut section: Option<NetSection> = None;
    let mut in_entry = false;
    for line in text::lines(output) {
        if line.starts_with(|c| !text::is_space(c)) {
            section = match text::trim(line) {
                "Xt stats:" => Some(NetSection::Xt),
                "UID stats:" => Some(NetSection::Uid),
                _ => None,
            };
            in_entry = false;
            continue;
        }
        let Some(section) = section else {
            continue;
        };
        if let Some(ident) = ident_line(line) {
            entries.push(ident.entry(section));
            in_entry = true;
            continue;
        }
        let Some(current) = entries.last_mut().filter(|_| in_entry) else {
            continue;
        };
        if let Some(seconds) =
            text::number_after(line, "bucketDuration=", Sign::Unsigned, Start::Anywhere)
        {
            current.bucket_seconds = saturating_i64(seconds);
            continue;
        }
        if let Some(bucket) = bucket_line(line) {
            current.buckets.push(bucket);
        }
    }
    entries
}

/// The parts of `/^\s+ident=\[(.*)\] uid=(-?\d+) set=(\S+) tag=(\S+)/`.
struct IdentLine<'line> {
    identities: &'line str,
    uid: i64,
    tag: &'line str,
}

impl IdentLine<'_> {
    fn entry(&self, section: NetSection) -> NetEntry {
        let bodies = identity_bodies(self.identities);
        let sub_id = bodies
            .iter()
            .find_map(|body| identity_field(body, "subId"))
            .and_then(text::parse_int);
        NetEntry {
            section,
            mobile: bodies.iter().any(|body| has_word(body, "type=0")),
            metered: bodies.iter().any(|body| has_word(body, "metered=true")),
            sub_id,
            uid: self.uid,
            tag: self.tag.to_owned(),
            bucket_seconds: 0,
            buckets: Vec::new(),
        }
    }
}

fn ident_line(line: &str) -> Option<IdentLine<'_>> {
    const SEPARATOR: &str = "] uid=";
    let rest = text::trim_start(line);
    if rest.len() == line.len() {
        return None;
    }
    let after = rest.strip_prefix("ident=[")?;
    let mut candidates: Vec<usize> = text::occurrences(after, SEPARATOR).collect();
    candidates.reverse();
    candidates.into_iter().find_map(|index| {
        let identities = &after[..index];
        if identities.contains(text::is_line_terminator) {
            return None;
        }
        let tail = &after[index + SEPARATOR.len()..];
        let uid = text::leading_number(tail, Sign::Signed)?;
        let tail = tail[uid.len()..].strip_prefix(" set=")?;
        let set = text::leading_non_space(tail);
        let tail = tail[set.len()..].strip_prefix(" tag=")?;
        let tag = text::leading_non_space(tail);
        if set.is_empty() || tag.is_empty() {
            return None;
        }
        Some(IdentLine {
            identities,
            uid: text::parse_int(uid)?,
            tag,
        })
    })
}

/// The bodies of `/\{([^}]*)\}/g`: each `{` up to the next `}`.
fn identity_bodies(identities: &str) -> Vec<&str> {
    let mut bodies = Vec::new();
    let mut rest = identities;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            break;
        };
        bodies.push(&after[..close]);
        rest = &after[close + 1..];
    }
    bodies
}

/// The value in the first `/\b<name>=([^,\s]+)/` of an identity.
fn identity_field<'body>(body: &'body str, name: &str) -> Option<&'body str> {
    let prefix = format!("{name}=");
    text::occurrences(body, &prefix).find_map(|index| {
        if !text::word_boundary_before(body, index) {
            return None;
        }
        let value = &body[index + prefix.len()..];
        let end = value
            .find(|c: char| c == ',' || text::is_space(c))
            .unwrap_or(value.len());
        (end > 0).then(|| &value[..end])
    })
}

/// Whether `/\b<word>\b/` matches.
fn has_word(body: &str, word: &str) -> bool {
    text::occurrences(body, word).any(|index| {
        text::word_boundary_before(body, index)
            && text::word_boundary_after(body, index + word.len())
    })
}

/// `/^\s+st=(\d+) rb=(\d+) rp=\d+ tb=(\d+)/`.
fn bucket_line(line: &str) -> Option<NetBucket> {
    let rest = text::trim_start(line);
    if rest.len() == line.len() {
        return None;
    }
    let mut numbers = [""; 4];
    let mut tail = rest;
    for (number, label) in numbers.iter_mut().zip(["st=", " rb=", " rp=", " tb="]) {
        tail = tail.strip_prefix(label)?;
        *number = text::leading_number(tail, Sign::Unsigned)?;
        tail = &tail[number.len()..];
    }
    let [start, rx, _, tx] = numbers;
    Some(NetBucket {
        start: saturating_i64(start),
        rx: saturating_u64(rx),
        tx: saturating_u64(tx),
    })
}

/// The value of a run of digits, or `i64::MAX` when it is too large.
fn saturating_i64(digits: &str) -> i64 {
    digits.parse().unwrap_or(i64::MAX)
}

/// The value of a run of digits, or `u64::MAX` when it is too large.
fn saturating_u64(digits: &str) -> u64 {
    digits.parse().unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uid_entry(ident: &str) -> Option<NetEntry> {
        parse_netstats(&format!("UID stats:\n{ident}\n"))
            .into_iter()
            .next()
    }

    #[test]
    fn unparsable_text_has_no_entries() {
        assert_eq!(parse_netstats(""), []);
        assert_eq!(parse_netstats("Can't find service: netstats"), []);
        assert_eq!(
            parse_netstats("Dev stats:\n  ident=[{type=0}] uid=-1 set=ALL tag=0x0\n"),
            []
        );
    }

    #[test]
    fn ident_lines() {
        let entry = uid_entry(
            "  ident=[{type=0, ratType=13, metered=true, subId=1}, {type=1, subId=2}] uid=10101 \
             set=DEFAULT tag=0x0 extra",
        )
        .expect("an entry");
        assert_eq!(
            entry,
            NetEntry {
                section: NetSection::Uid,
                mobile: true,
                metered: true,
                sub_id: Some(1),
                uid: 10101,
                tag: "0x0".to_owned(),
                bucket_seconds: 0,
                buckets: Vec::new(),
            }
        );
        let other = uid_entry("  ident=[{type=01, subtype=0, subId=x}] uid=-1 set=ALL tag=0x1")
            .expect("an entry");
        assert!(!other.mobile, "type=01 isn't type 0");
        assert_eq!(other.sub_id, None);
        assert_eq!(uid_entry("ident=[{type=0}] uid=1 set=A tag=B"), None);
        assert_eq!(uid_entry("  ident=[{type=0}] uid=x set=A tag=B"), None);
    }

    #[test]
    fn the_last_separator_that_fits_ends_the_identities() {
        let entry = uid_entry("  ident=[{subId=1}] uid=2 set=A tag=B] uid=3 set=C tag=D")
            .expect("an entry");
        assert_eq!(entry.uid, 3);
        assert_eq!(entry.tag, "D");
        let entry =
            uid_entry("  ident=[{subId=1}] uid=2 set=A tag=B] uid=3 set=C").expect("an entry");
        assert_eq!(entry.uid, 2);
    }

    #[test]
    fn identity_fields_take_the_first_identity_that_has_one() {
        assert_eq!(identity_field("xsubId=1, subId=2", "subId"), Some("2"));
        assert_eq!(identity_field("subId=, subId=3", "subId"), Some("3"));
        assert_eq!(identity_bodies("{a}{b{c}d}{e"), ["a", "b{c"]);
    }

    #[test]
    fn buckets_belong_to_the_entry_above_them() {
        let entries = parse_netstats(
            "UID stats:\n    st=1 rb=2 rp=3 tb=4\n  ident=[{type=0}] uid=0 set=A tag=0x0\n    \
             NetworkStatsHistory: bucketDuration=7200\n      st=10 rb=20 rp=30 tb=40 tp=1\n      \
             st=11 rb=x rp=3 tb=4\nXt stats:\n      st=99 rb=1 rp=1 tb=1\n",
        );
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].bucket_seconds, 7200);
        assert_eq!(
            entries[0].buckets,
            [NetBucket {
                start: 10,
                rx: 20,
                tx: 40,
            }]
        );
    }

    #[test]
    fn numbers_too_large_saturate() {
        let entries = parse_netstats(
            "UID stats:\n  ident=[{type=0}] uid=0 set=A tag=0x0\n    \
             st=99999999999999999999 rb=99999999999999999999 rp=0 tb=0\n",
        );
        assert_eq!(entries[0].buckets[0].start, i64::MAX);
        assert_eq!(entries[0].buckets[0].rx, u64::MAX);
    }
}
