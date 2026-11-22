//! The health monitor's memory: 5-minute aggregates per phone, the hot-battery and
//! out-of-service spells, and the alerts they raise.

use std::collections::BTreeMap;
use std::mem;
use std::time::Duration;

use crate::monitor::alert::{Alert, AlertKind};
use crate::monitor::check_report::MobileBytes;
use crate::monitor::severity::Severity;
use crate::{Battery, PhoneHealth, PhoneStatus, ServiceState, Timestamp};

/// How long a phone may be away before the offline alert.
pub const OFFLINE_ALERT: Duration = Duration::from_mins(5);
/// How long a loaded SIM may be out of service before its alert.
pub const OUT_OF_SERVICE_ALERT: Duration = Duration::from_mins(30);
/// The hot-battery alert raises above 45.0 °C, in tenths of a degree.
pub const HOT_BATTERY_DECI_C: i64 = 450;
/// ...and clears below 43.0 °C, so a battery hovering at the limit does not flap.
pub const COOL_BATTERY_DECI_C: i64 = 430;
/// In-service samples in a row that end an out-of-service spell.
pub const IN_SERVICE_SAMPLES_TO_CLEAR: u32 = 3;
/// The length of a `health_5m` bucket.
pub const BUCKET: Duration = Duration::from_mins(5);
/// How long `health_5m` rows are kept.
pub const RETENTION: Duration = Duration::from_hours(90 * 24);
/// How often old `health_5m` rows are deleted, at most.
pub const PRUNE_INTERVAL: Duration = Duration::from_hours(24);

const BUCKET_MILLIS: i64 = 5 * 60 * 1000;

/// One phone as the health monitor sees it.
#[derive(Clone, Copy, Debug)]
pub struct PhoneSample<'phone> {
    /// The phone's serial.
    pub serial: &'phone str,
    /// Its connection state.
    pub status: PhoneStatus,
    /// When the state last changed.
    pub since: Timestamp,
    /// Its latest health probe, if any.
    pub health: Option<&'phone PhoneHealth>,
}

/// One `health_5m` row, for the server to write with `INSERT OR REPLACE` under the phone's id.
/// The server skips a phone with no row in `phones`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HealthRow {
    /// The phone's serial.
    pub serial: String,
    /// The bucket's start: the sample time rounded down to 5 minutes.
    pub bucket: Timestamp,
    /// Samples in the bucket.
    pub samples: u64,
    /// The mean battery level, rounded as `Math.round` does, or `None` without battery readings.
    pub battery_pct: Option<i64>,
    /// The highest battery temperature in tenths of a degree.
    pub battery_temp_dc: Option<i64>,
    /// `{"<slot>":{"minLevel":int,"dbm":int|null}}`, slots ascending, `dbm` the rounded mean.
    pub signal_json: String,
    /// `{"<slot>":share}`, the share of samples in service rounded to 2 decimals. Only slots
    /// with a loaded SIM count.
    pub service_json: String,
    /// Mobile bytes received at the phone's latest check, if it had one.
    pub mobile_rx: Option<u64>,
    /// Mobile bytes sent at the phone's latest check, if it had one.
    pub mobile_tx: Option<u64>,
}

/// What one sample produced.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HealthSampleOutput {
    /// The buckets that closed, to write.
    pub rows: Vec<HealthRow>,
    /// Delete the `health_5m` rows whose bucket is before this time, when it's time to prune.
    pub prune_before: Option<Timestamp>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct SlotAggregate {
    min_level: i64,
    dbm_sum: i64,
    dbm_count: u64,
    in_service: u64,
    samples: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Bucket {
    start: Timestamp,
    samples: u64,
    battery_sum: i64,
    battery_count: u64,
    temp_max: Option<i64>,
    slots: BTreeMap<u32, SlotAggregate>,
}

impl Bucket {
    fn new(start: Timestamp) -> Self {
        Self {
            start,
            samples: 0,
            battery_sum: 0,
            battery_count: 0,
            temp_max: None,
            slots: BTreeMap::new(),
        }
    }

    fn row(&self, serial: &str, mobile: Option<MobileBytes>) -> HealthRow {
        use std::fmt::Write as _;

        let mut signal = String::from("{");
        let mut service = String::from("{");
        for (index, (slot, value)) in self.slots.iter().enumerate() {
            let separator = if index == 0 { "" } else { "," };
            let dbm = if value.dbm_count > 0 {
                js_round(ratio(value.dbm_sum, value.dbm_count)).to_string()
            } else {
                "null".to_owned()
            };
            let share = round_half_up(ratio_u(value.in_service, value.samples) * 100.0) / 100.0;
            write!(
                signal,
                "{separator}\"{slot}\":{{\"minLevel\":{},\"dbm\":{dbm}}}",
                value.min_level
            )
            .expect("writing to a String never fails");
            write!(service, "{separator}\"{slot}\":{share}")
                .expect("writing to a String never fails");
        }
        signal.push('}');
        service.push('}');
        HealthRow {
            serial: serial.to_owned(),
            bucket: self.start,
            samples: self.samples,
            battery_pct: (self.battery_count > 0)
                .then(|| js_round(ratio(self.battery_sum, self.battery_count))),
            battery_temp_dc: self.temp_max,
            signal_json: signal,
            service_json: service,
            mobile_rx: mobile.map(|bytes| bytes.rx),
            mobile_tx: mobile.map(|bytes| bytes.tx),
        }
    }
}

#[expect(
    clippy::cast_precision_loss,
    reason = "the old monitor averaged in doubles; health sums are far below 2^53"
)]
fn ratio(sum: i64, count: u64) -> f64 {
    sum as f64 / count as f64
}

#[expect(
    clippy::cast_precision_loss,
    reason = "the old monitor averaged in doubles; sample counts are far below 2^53"
)]
fn ratio_u(part: u64, whole: u64) -> f64 {
    part as f64 / whole as f64
}

/// `Math.round`: the nearest integer, halves toward positive infinity.
fn round_half_up(value: f64) -> f64 {
    let floor = value.floor();
    if value - floor >= 0.5 {
        floor + 1.0
    } else {
        floor
    }
}

/// `Math.round` of a mean, as an integer.
#[expect(
    clippy::cast_possible_truncation,
    reason = "the values are means of small integers, well inside i64"
)]
fn js_round(value: f64) -> i64 {
    round_half_up(value) as i64
}

/// A loaded SIM that isn't in service: when it started, and the in-service samples in a row
/// since.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Spell {
    key: String,
    since: Timestamp,
    in_service_streak: u32,
}

/// A battery over the limit: when it went over, and its last reading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Hot {
    since: Timestamp,
    temperature_deci_c: i64,
}

/// The health monitor's memory, fed one sample of every phone per health interval.
///
/// It keeps the 5-minute aggregate of each online phone in memory and hands back one row per
/// phone per bucket when the next sample falls in a new bucket, which keeps SD card writes small.
/// A phone that goes offline keeps its bucket until it comes back online, or until
/// [`HealthTracker::flush`] at shutdown.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HealthTracker {
    buckets: BTreeMap<String, Bucket>,
    out_of_service: Vec<Spell>,
    hot: BTreeMap<String, Hot>,
    last_prune: Option<Timestamp>,
}

impl HealthTracker {
    /// A tracker with nothing recorded.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            buckets: BTreeMap::new(),
            out_of_service: Vec::new(),
            hot: BTreeMap::new(),
            last_prune: None,
        }
    }

    /// Records one sample of every phone at `at`. `mobile_bytes` gives a phone's mobile bytes at
    /// its latest check, for the rows that close.
    pub fn sample(
        &mut self,
        phones: &[PhoneSample<'_>],
        at: Timestamp,
        mobile_bytes: impl Fn(&str) -> Option<MobileBytes>,
    ) -> HealthSampleOutput {
        let start = Timestamp::from_millis(at.millis().div_euclid(BUCKET_MILLIS) * BUCKET_MILLIS);
        let mut output = HealthSampleOutput::default();
        for phone in phones {
            let serial = phone.serial;
            let health = match phone.health {
                Some(health) if phone.status == PhoneStatus::Online => health,
                _ => {
                    let prefix = format!("{serial}:");
                    self.out_of_service
                        .retain(|spell| !spell.key.starts_with(&prefix));
                    self.hot.remove(serial);
                    continue;
                }
            };
            if let Some(bucket) = self.buckets.get(serial)
                && bucket.start != start
            {
                output.rows.push(bucket.row(serial, mobile_bytes(serial)));
                self.buckets.remove(serial);
            }
            let bucket = self
                .buckets
                .entry(serial.to_owned())
                .or_insert_with(|| Bucket::new(start));
            bucket.samples += 1;
            if let Some(battery) = &health.battery {
                record_battery(bucket, &mut self.hot, serial, battery, at);
            }
            for radio in &health.radios {
                let key = format!("{serial}:{}", radio.slot);
                let loaded = usize::try_from(radio.slot)
                    .ok()
                    .and_then(|slot| health.sim_states.get(slot))
                    .is_some_and(|state| state == "LOADED");
                if !loaded {
                    self.out_of_service.retain(|spell| spell.key != key);
                    continue;
                }
                let slot = bucket
                    .slots
                    .entry(radio.slot)
                    .or_insert_with(|| SlotAggregate {
                        min_level: radio.signal_level,
                        ..SlotAggregate::default()
                    });
                slot.samples += 1;
                slot.min_level = slot.min_level.min(radio.signal_level);
                if let Some(dbm) = radio.signal_dbm {
                    slot.dbm_sum += dbm;
                    slot.dbm_count += 1;
                }
                let spell = self
                    .out_of_service
                    .iter()
                    .position(|spell| spell.key == key);
                if radio.voice == ServiceState::InService {
                    slot.in_service += 1;
                    if let Some(index) = spell {
                        self.out_of_service[index].in_service_streak += 1;
                        if self.out_of_service[index].in_service_streak
                            >= IN_SERVICE_SAMPLES_TO_CLEAR
                        {
                            self.out_of_service.remove(index);
                        }
                    }
                } else if let Some(index) = spell {
                    self.out_of_service[index].in_service_streak = 0;
                } else {
                    self.out_of_service.push(Spell {
                        key,
                        since: at,
                        in_service_streak: 0,
                    });
                }
            }
        }
        let last_prune = self.last_prune.map_or(0, Timestamp::millis);
        if at.millis() - last_prune > millis(PRUNE_INTERVAL) {
            self.last_prune = Some(at);
            output.prune_before = Some(at.minus(RETENTION));
        }
        output
    }

    /// The partial buckets, to write at shutdown; the tracker forgets them.
    pub fn flush(&mut self, mobile_bytes: impl Fn(&str) -> Option<MobileBytes>) -> Vec<HealthRow> {
        let buckets = mem::take(&mut self.buckets);
        buckets
            .iter()
            .map(|(serial, bucket)| bucket.row(serial, mobile_bytes(serial)))
            .collect()
    }

    /// The health alerts at `now`, in the order of `phones`, then the out-of-service spells in
    /// the order they started: no permissions, offline for over 5 minutes, a hot battery, and a
    /// loaded SIM out of service for over 30 minutes.
    #[must_use]
    pub fn alerts(&self, phones: &[PhoneSample<'_>], now: Timestamp) -> Vec<Alert> {
        let mut alerts = Vec::new();
        for phone in phones {
            let serial = phone.serial;
            match phone.status {
                PhoneStatus::NoPermissions => {
                    alerts.push(Alert {
                        key: format!("no_permissions:{serial}"),
                        kind: AlertKind::NoPermissions,
                        serial: serial.to_owned(),
                        severity: Severity::Critical,
                        message: "The host's adb server cannot open this USB device (udev rules \
                                  or plugdev group)."
                            .to_owned(),
                        since: phone.since,
                    });
                    continue;
                }
                PhoneStatus::Online => {}
                PhoneStatus::Connecting
                | PhoneStatus::NeedsAuth
                | PhoneStatus::Offline
                | PhoneStatus::Other => {
                    if now.millis() - phone.since.millis() > millis(OFFLINE_ALERT) {
                        alerts.push(Alert {
                            key: format!("offline:{serial}"),
                            kind: AlertKind::Offline,
                            serial: serial.to_owned(),
                            severity: Severity::Critical,
                            message: if phone.status == PhoneStatus::NeedsAuth {
                                "Waiting for Allow USB debugging for more than 5 minutes."
                            } else {
                                "Offline for more than 5 minutes."
                            }
                            .to_owned(),
                            since: phone.since,
                        });
                    }
                    continue;
                }
            }
            if let Some(hot) = self.hot.get(serial) {
                alerts.push(Alert {
                    key: format!("battery_hot:{serial}"),
                    kind: AlertKind::BatteryHot,
                    serial: serial.to_owned(),
                    severity: Severity::Critical,
                    message: format!(
                        "Battery at {} °C (over 45 °C).",
                        tenths(hot.temperature_deci_c)
                    ),
                    since: hot.since,
                });
            }
        }
        for spell in &self.out_of_service {
            if now.millis() - spell.since.millis() <= millis(OUT_OF_SERVICE_ALERT) {
                continue;
            }
            let mut parts = spell.key.split(':');
            let serial = parts.next().unwrap_or_default();
            let slot = parts.next().unwrap_or_default();
            alerts.push(Alert {
                key: format!("out_of_service:{}", spell.key),
                kind: AlertKind::OutOfService,
                serial: serial.to_owned(),
                severity: Severity::Warning,
                message: format!(
                    "SIM {} out of service for more than 30 minutes.",
                    plus_one(slot)
                ),
                since: spell.since,
            });
        }
        alerts
    }
}

fn record_battery(
    bucket: &mut Bucket,
    hot: &mut BTreeMap<String, Hot>,
    serial: &str,
    battery: &Battery,
    at: Timestamp,
) {
    let temperature = battery.temperature_deci_c;
    bucket.battery_sum += battery.level;
    bucket.battery_count += 1;
    bucket.temp_max = Some(
        bucket
            .temp_max
            .map_or(temperature, |max| max.max(temperature)),
    );
    let previous = hot.get(serial).copied();
    if temperature > HOT_BATTERY_DECI_C
        || (previous.is_some() && temperature >= COOL_BATTERY_DECI_C)
    {
        hot.insert(
            serial.to_owned(),
            Hot {
                since: previous.map_or(at, |previous| previous.since),
                temperature_deci_c: temperature,
            },
        );
    } else {
        hot.remove(serial);
    }
}

fn millis(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis()).expect("the monitor's durations fit in i64 milliseconds")
}

/// Tenths of a degree with one decimal, as `(deciC / 10).toFixed(1)` writes them.
fn tenths(deci: i64) -> String {
    let sign = if deci < 0 { "-" } else { "" };
    let magnitude = deci.unsigned_abs();
    format!("{sign}{}.{}", magnitude / 10, magnitude % 10)
}

/// `Number(slot) + 1` for the slot part of a spell's key. A serial with a colon in it, such as
/// `192.168.1.5:5555`, splits there, as it did in the old monitor.
fn plus_one(slot: &str) -> String {
    if slot.is_empty() {
        return "1".to_owned();
    }
    slot.parse::<i64>()
        .map_or_else(|_| "NaN".to_owned(), |slot| (slot + 1).to_string())
}

#[cfg(test)]
mod tests {
    use regex::Regex;

    use super::*;

    const HEALTH: &str = include_str!("../../tests/fixtures/nuu-s6702x/health.txt");
    const IN_SERVICE: &str = include_str!("../../tests/fixtures/nuu-s6702x/health-in-service.txt");
    const SERIAL: &str = "B30";

    fn probe(text: &str) -> PhoneHealth {
        crate::parse_health(text)
    }

    fn with_temperature(deci: i64) -> PhoneHealth {
        let pattern = Regex::new(r"temperature: [0-9]+").unwrap();
        probe(&pattern.replace(HEALTH, format!("temperature: {deci}")))
    }

    fn online(health: &PhoneHealth) -> PhoneSample<'_> {
        PhoneSample {
            serial: SERIAL,
            status: PhoneStatus::Online,
            since: at(0),
            health: Some(health),
        }
    }

    fn at(minutes: i64) -> Timestamp {
        Timestamp::parse_iso("2026-10-22T08:00:00.000Z")
            .unwrap()
            .plus(Duration::from_secs(u64::try_from(minutes * 60).unwrap()))
    }

    fn no_check(_: &str) -> Option<MobileBytes> {
        None
    }

    fn kinds(alerts: &[Alert]) -> Vec<AlertKind> {
        alerts.iter().map(|alert| alert.kind).collect()
    }

    #[test]
    fn five_minute_aggregates_close_when_the_next_sample_falls_in_a_new_bucket() {
        let recorded = probe(HEALTH);
        let mut tracker = HealthTracker::new();
        let first = tracker.sample(&[online(&recorded)], at(0), no_check);
        assert_eq!(first.rows, []);
        let second = tracker.sample(&[online(&recorded)], at(5), no_check);
        assert_eq!(
            second.rows,
            [HealthRow {
                serial: SERIAL.to_owned(),
                bucket: at(0),
                samples: 1,
                battery_pct: Some(85),
                battery_temp_dc: recorded
                    .battery
                    .as_ref()
                    .map(|battery| battery.temperature_deci_c),
                signal_json: second.rows[0].signal_json.clone(),
                service_json: "{\"0\":0}".to_owned(),
                mobile_rx: None,
                mobile_tx: None,
            }]
        );
        assert!(
            second.rows[0]
                .signal_json
                .starts_with("{\"0\":{\"minLevel\":")
        );
        let flushed = tracker.flush(|_| Some(MobileBytes { rx: 7, tx: 3 }));
        assert_eq!(flushed.len(), 1);
        assert_eq!(flushed[0].bucket, at(5));
        assert_eq!(
            (flushed[0].mobile_rx, flushed[0].mobile_tx),
            (Some(7), Some(3))
        );
        assert_eq!(tracker.flush(no_check), []);
    }

    #[test]
    fn a_bucket_rounds_its_means_and_shares_as_the_old_monitor_did() {
        let mut bucket = Bucket::new(at(0));
        bucket.samples = 3;
        bucket.battery_sum = 85 + 86;
        bucket.battery_count = 2;
        bucket.temp_max = Some(301);
        bucket.slots.insert(
            1,
            SlotAggregate {
                min_level: 2,
                dbm_sum: -171,
                dbm_count: 2,
                in_service: 2,
                samples: 3,
            },
        );
        bucket.slots.insert(
            0,
            SlotAggregate {
                min_level: 0,
                dbm_sum: 0,
                dbm_count: 0,
                in_service: 3,
                samples: 3,
            },
        );
        let row = bucket.row(SERIAL, Some(MobileBytes { rx: 0, tx: 0 }));
        assert_eq!(row.battery_pct, Some(86), "85.5 rounds up");
        assert_eq!(
            row.signal_json,
            "{\"0\":{\"minLevel\":0,\"dbm\":null},\"1\":{\"minLevel\":2,\"dbm\":-85}}",
            "-85.5 rounds toward positive infinity"
        );
        assert_eq!(row.service_json, "{\"0\":1,\"1\":0.67}");
        assert_eq!((row.mobile_rx, row.mobile_tx), (Some(0), Some(0)));
    }

    #[test]
    fn the_first_sample_prunes_then_at_most_once_a_day() {
        let recorded = probe(HEALTH);
        let mut tracker = HealthTracker::new();
        assert_eq!(
            tracker
                .sample(&[online(&recorded)], at(0), no_check)
                .prune_before,
            Some(at(0).minus(RETENTION))
        );
        assert_eq!(
            tracker.sample(&[], at(60 * 24), no_check).prune_before,
            None
        );
        assert_eq!(
            tracker.sample(&[], at(60 * 24 + 1), no_check).prune_before,
            Some(at(60 * 24 + 1).minus(RETENTION))
        );
    }

    #[test]
    fn the_offline_alert_waits_5_minutes() {
        let tracker = HealthTracker::new();
        let offline = |status| PhoneSample {
            serial: SERIAL,
            status,
            since: at(0),
            health: None,
        };
        assert_eq!(tracker.alerts(&[offline(PhoneStatus::Offline)], at(5)), []);
        let alerts = tracker.alerts(&[offline(PhoneStatus::Offline)], at(6));
        assert_eq!(kinds(&alerts), [AlertKind::Offline]);
        assert_eq!(alerts[0].key, "offline:B30");
        assert_eq!(alerts[0].message, "Offline for more than 5 minutes.");
        assert_eq!(alerts[0].since, at(0));
        assert_eq!(
            tracker.alerts(&[offline(PhoneStatus::NeedsAuth)], at(6))[0].message,
            "Waiting for Allow USB debugging for more than 5 minutes."
        );
        let no_permissions = tracker.alerts(&[offline(PhoneStatus::NoPermissions)], at(0));
        assert_eq!(
            no_permissions[0].message,
            "The host's adb server cannot open this USB device (udev rules or plugdev group)."
        );
        assert_eq!(no_permissions[0].key, "no_permissions:B30");
    }

    #[test]
    fn the_hot_battery_alert_raises_above_45_c_and_clears_below_43_c() {
        let mut tracker = HealthTracker::new();
        let mut hot_at = |deci: i64, minute: i64| {
            let probe = with_temperature(deci);
            let phones = [online(&probe)];
            tracker.sample(&phones, at(minute), no_check);
            tracker
                .alerts(&phones, at(minute))
                .into_iter()
                .find(|alert| alert.kind == AlertKind::BatteryHot)
        };
        assert_eq!(hot_at(450, 0), None);
        let raised = hot_at(468, 1).unwrap();
        assert_eq!(raised.message, "Battery at 46.8 °C (over 45 °C).");
        assert_eq!(raised.since, at(1));
        let kept = hot_at(440, 2).unwrap();
        assert_eq!(kept.message, "Battery at 44.0 °C (over 45 °C).");
        assert_eq!(kept.since, at(1));
        assert_eq!(hot_at(429, 3), None);
    }

    #[test]
    fn out_of_service_for_30_minutes_alerts_and_three_in_service_samples_clear_it() {
        let out = probe(HEALTH);
        let inside = probe(IN_SERVICE);
        let mut tracker = HealthTracker::new();
        let mut minute = 0;
        let mut sample = |probe: &PhoneHealth, minutes: i64| {
            let phones = [online(probe)];
            minute += minutes;
            tracker.sample(&phones, at(minute), no_check);
            tracker
                .alerts(&phones, at(minute))
                .into_iter()
                .find(|alert| alert.kind == AlertKind::OutOfService)
        };
        assert_eq!(sample(&out, 0), None);
        let alert = sample(&out, 31).unwrap();
        assert_eq!(alert.key, "out_of_service:B30:0");
        assert_eq!(
            alert.message,
            "SIM 1 out of service for more than 30 minutes."
        );
        assert_eq!(alert.severity, Severity::Warning);
        assert!(sample(&inside, 1).is_some());
        assert!(sample(&inside, 1).is_some());
        assert!(
            sample(&out, 1).is_some(),
            "one no-service sample restarts the count, but not the spell"
        );
        assert!(sample(&inside, 1).is_some());
        assert!(sample(&inside, 1).is_some());
        assert_eq!(sample(&inside, 1), None);
    }

    #[test]
    fn an_offline_phone_forgets_its_spells_but_keeps_its_bucket() {
        let out = probe(HEALTH);
        let mut tracker = HealthTracker::new();
        tracker.sample(&[online(&out)], at(0), no_check);
        let gone = PhoneSample {
            serial: SERIAL,
            status: PhoneStatus::Offline,
            since: at(1),
            health: None,
        };
        tracker.sample(&[gone], at(1), no_check);
        assert_eq!(tracker.alerts(&[online(&out)], at(40)), []);
        assert_eq!(tracker.flush(no_check).len(), 1);
    }

    #[test]
    fn a_serial_with_a_colon_splits_its_spell_key_as_the_old_monitor_did() {
        let mut tracker = HealthTracker::new();
        tracker.out_of_service.push(Spell {
            key: "192.168.1.5:5555:0".to_owned(),
            since: at(0),
            in_service_streak: 0,
        });
        let alerts = tracker.alerts(&[], at(31));
        assert_eq!(alerts[0].serial, "192.168.1.5");
        assert_eq!(alerts[0].key, "out_of_service:192.168.1.5:5555:0");
        assert_eq!(
            alerts[0].message,
            "SIM 5556 out of service for more than 30 minutes."
        );
    }

    #[test]
    fn tenths_and_rounding_match_javascript() {
        assert_eq!(tenths(468), "46.8");
        assert_eq!(tenths(440), "44.0");
        assert_eq!(tenths(-5), "-0.5");
        assert_eq!(js_round(2.5), 3);
        assert_eq!(js_round(-2.5), -2);
        assert_eq!(js_round(-85.5), -85);
        assert_eq!(js_round(0.499_999_999_999_999_94), 0);
    }
}
