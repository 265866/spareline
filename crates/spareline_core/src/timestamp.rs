//! Points in time as the database and the API spell them.

use std::fmt::{self, Display, Formatter};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MILLIS_PER_DAY: i64 = 86_400_000;

/// A point in time with millisecond precision, written exactly as JavaScript's
/// `Date.prototype.toISOString` writes it: `2026-10-28T08:41:51.104Z`, 24 characters, UTC.
///
/// Every time column in the database holds this form, and queries order and compare the strings,
/// so a row written in any other form, such as without milliseconds, would sort wrongly against
/// the rows spareline-old wrote.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp {
    millis: i64,
}

impl Timestamp {
    /// The time `millis` milliseconds after the Unix epoch.
    #[must_use]
    pub const fn from_millis(millis: i64) -> Self {
        Self { millis }
    }

    /// The current time, from the system clock.
    #[must_use]
    pub fn now() -> Self {
        Self::from_system_time(SystemTime::now())
    }

    /// The time `time` names, truncated to the millisecond. Times outside the years 0 to 9999
    /// clamp to the nearest end.
    #[must_use]
    pub fn from_system_time(time: SystemTime) -> Self {
        let millis = match time.duration_since(UNIX_EPOCH) {
            Ok(after) => i64::try_from(after.as_millis()).unwrap_or(i64::MAX),
            Err(before) => {
                i64::try_from(before.duration().as_millis()).map_or(i64::MIN, |millis| -millis)
            }
        };
        Self::from_millis(millis.clamp(MIN_MILLIS, MAX_MILLIS))
    }

    /// Milliseconds after the Unix epoch.
    #[must_use]
    pub const fn millis(self) -> i64 {
        self.millis
    }

    /// The time `duration` later, saturating at the end of the year 9999.
    #[must_use]
    pub fn plus(self, duration: Duration) -> Self {
        let delta = i64::try_from(duration.as_millis()).unwrap_or(i64::MAX);
        Self::from_millis(self.millis.saturating_add(delta).min(MAX_MILLIS))
    }

    /// The time `duration` earlier, saturating at the start of the year 0.
    #[must_use]
    pub fn minus(self, duration: Duration) -> Self {
        let delta = i64::try_from(duration.as_millis()).unwrap_or(i64::MAX);
        Self::from_millis(self.millis.saturating_sub(delta).max(MIN_MILLIS))
    }

    /// How long after `earlier` this is, or zero when it isn't later.
    #[must_use]
    pub fn since(self, earlier: Self) -> Duration {
        u64::try_from(self.millis.saturating_sub(earlier.millis))
            .map_or(Duration::ZERO, Duration::from_millis)
    }

    /// Reads the exact 24-character form [`Display`] writes, such as `2026-10-28T08:41:51.104Z`.
    /// Anything else, a missing millisecond part or an offset included, is `None`.
    #[must_use]
    pub fn parse_iso(text: &str) -> Option<Self> {
        let bytes = text.as_bytes();
        if bytes.len() != 24 {
            return None;
        }
        let separators = [
            (4, b'-'),
            (7, b'-'),
            (10, b'T'),
            (13, b':'),
            (16, b':'),
            (19, b'.'),
            (23, b'Z'),
        ];
        if separators.iter().any(|&(index, byte)| bytes[index] != byte) {
            return None;
        }
        let field = |start: usize, end: usize| -> Option<i64> {
            let digits = &bytes[start..end];
            if !digits.iter().all(u8::is_ascii_digit) {
                return None;
            }
            Some(
                digits
                    .iter()
                    .fold(0, |value, digit| value * 10 + i64::from(digit - b'0')),
            )
        };
        let year = field(0, 4)?;
        let month = field(5, 7)?;
        let day = field(8, 10)?;
        let hour = field(11, 13)?;
        let minute = field(14, 16)?;
        let second = field(17, 19)?;
        let milli = field(20, 23)?;
        if !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) {
            return None;
        }
        if hour > 23 || minute > 59 || second > 59 {
            return None;
        }
        let days = days_from_civil(year, month, day);
        let millis = days * MILLIS_PER_DAY + ((hour * 60 + minute) * 60 + second) * 1000 + milli;
        Some(Self::from_millis(millis))
    }

    /// Reads a calendar date, `YYYY-MM-DD`, as midnight UTC that day.
    #[must_use]
    pub fn parse_date(text: &str) -> Option<Self> {
        Self::parse_iso(&format!("{text}T00:00:00.000Z"))
    }
}

/// The earliest time the 24-character form can hold: the start of the year 0.
const MIN_MILLIS: i64 = -62_167_219_200_000;

/// The latest time the 24-character form can hold: the end of the year 9999.
const MAX_MILLIS: i64 = 253_402_300_799_999;

impl Display for Timestamp {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let days = self.millis.div_euclid(MILLIS_PER_DAY);
        let in_day = self.millis.rem_euclid(MILLIS_PER_DAY);
        let CivilDate { year, month, day } = civil_from_days(days);
        let hour = in_day / 3_600_000;
        let minute = in_day / 60_000 % 60;
        let second = in_day / 1000 % 60;
        let milli = in_day % 1000;
        write!(
            f,
            "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{milli:03}Z"
        )
    }
}

/// A proleptic Gregorian date.
struct CivilDate {
    year: i64,
    month: i64,
    day: i64,
}

fn is_leap(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        2 if is_leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since 1970-01-01 of a proleptic Gregorian date, by Howard Hinnant's `days_from_civil`.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The proleptic Gregorian date of a day count since 1970-01-01, by Howard Hinnant's
/// `civil_from_days`.
fn civil_from_days(days: i64) -> CivilDate {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    CivilDate { year, month, day }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_what_javascript_writes() {
        let cases = [
            (0, "1970-01-01T00:00:00.000Z"),
            (1_792_540_800_000, "2026-10-21T00:00:00.000Z"),
            (1_793_176_911_104, "2026-10-28T08:41:51.104Z"),
            (951_782_400_000, "2000-02-29T00:00:00.000Z"),
            (-1, "1969-12-31T23:59:59.999Z"),
            (MAX_MILLIS, "9999-12-31T23:59:59.999Z"),
            (MIN_MILLIS, "0000-01-01T00:00:00.000Z"),
        ];
        for (millis, text) in cases {
            let time = Timestamp::from_millis(millis);
            assert_eq!(time.to_string(), text);
            assert_eq!(Timestamp::parse_iso(text), Some(time), "{text} reads back");
        }
    }

    #[test]
    fn reads_only_the_exact_form() {
        for text in [
            "2026-10-28T08:41:51Z",
            "2026-10-28T08:41:51.104+00:00",
            "2026-10-28 08:41:51.104Z",
            "2026-02-30T00:00:00.000Z",
            "2026-13-01T00:00:00.000Z",
            "2026-10-28T24:00:00.000Z",
            "２026-10-28T08:41:51.104Z",
        ] {
            assert_eq!(Timestamp::parse_iso(text), None, "{text} is refused");
        }
    }

    #[test]
    fn orders_like_its_text() {
        let earlier = Timestamp::parse_iso("2026-10-28T08:41:51.104Z").unwrap();
        let later = earlier.plus(Duration::from_millis(1));
        assert!(earlier < later);
        assert!(earlier.to_string() < later.to_string());
        assert_eq!(later.since(earlier), Duration::from_millis(1));
        assert_eq!(earlier.since(later), Duration::ZERO);
        assert_eq!(later.minus(Duration::from_millis(1)), earlier);
    }

    #[test]
    fn reads_a_calendar_date_as_midnight() {
        assert_eq!(
            Timestamp::parse_date("2026-10-21"),
            Some(Timestamp::from_millis(1_792_540_800_000))
        );
        assert_eq!(Timestamp::parse_date("2026-10-21T00:00"), None);
    }
}
