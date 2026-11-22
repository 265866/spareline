//! The battery service's state, from `dumpsys battery`.

use std::collections::HashMap;

use serde::{Serialize, Serializer};

use crate::text;

/// The battery's state, from `dumpsys battery`.
///
/// It serializes as the API's `Battery`: `level`, `temperatureC` (degrees Celsius, from the
/// tenths the service reports), `status` and `plugged`. The voltage and the health stay out of
/// the API, as they did in spareline-old.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Battery {
    /// The charge in percent of the reported scale, rounded half up.
    pub level: i64,
    /// Tenths of a degree Celsius, as the battery service reports it.
    #[serde(rename = "temperatureC", serialize_with = "serialize_celsius")]
    pub temperature_deci_c: i64,
    /// The voltage in millivolts, when reported.
    #[serde(skip)]
    pub voltage_mv: Option<i64>,
    /// Whether it is charging.
    pub status: BatteryStatus,
    /// The battery's health.
    #[serde(skip)]
    pub health: BatteryHealth,
    /// The power sources plugged in, in the order AC, USB, wireless, dock.
    pub plugged: Vec<PowerSource>,
}

/// `BatteryManager`'s `BATTERY_STATUS_*` values, serialized as their snake-case names, such as
/// `not_charging`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BatteryStatus {
    /// 1, or a value this doesn't name.
    Unknown,
    /// 2.
    Charging,
    /// 3.
    Discharging,
    /// 4: plugged in but not charging, such as when held at a charge limit.
    NotCharging,
    /// 5.
    Full,
}

/// `BatteryManager`'s `BATTERY_HEALTH_*` values, serialized as their snake-case names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BatteryHealth {
    /// 1, or a value this doesn't name.
    Unknown,
    /// 2.
    Good,
    /// 3.
    Overheat,
    /// 4.
    Dead,
    /// 5.
    OverVoltage,
    /// 6.
    Failure,
    /// 7.
    Cold,
}

/// A power source the battery service reports as plugged in, serialized in lower case.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PowerSource {
    /// `AC powered: true`.
    Ac,
    /// `USB powered: true`.
    Usb,
    /// `Wireless powered: true`.
    Wireless,
    /// `Dock powered: true`.
    Dock,
}

/// Reads the output of `dumpsys battery`. It is `None` when the level or the temperature is
/// missing or not an integer, such as when the service doesn't exist.
///
/// Each `  Key: value` line is read with its key in lower case; a key seen twice takes its last
/// value. A missing scale counts as 100, and a scale of zero or less leaves the level as
/// reported.
#[must_use]
pub fn parse_battery(text: &str) -> Option<Battery> {
    let mut fields: HashMap<String, String> = HashMap::new();
    for line in text::lines(text) {
        if let Some(field) = BatteryField::read(line) {
            fields.insert(field.key, field.value);
        }
    }
    let field = |key: &str| fields.get(key).map(String::as_str);
    let int = |key: &str| field(key).and_then(|value| text::parse_int(text::trim(value)));
    let level = int("level")?;
    let temperature = int("temperature")?;
    let scale = int("scale").unwrap_or(100);
    let powered = |key: &str| field(key) == Some("true");
    let plugged: Vec<PowerSource> = [
        ("ac powered", PowerSource::Ac),
        ("usb powered", PowerSource::Usb),
        ("wireless powered", PowerSource::Wireless),
        ("dock powered", PowerSource::Dock),
    ]
    .into_iter()
    .filter(|&(key, _)| powered(key))
    .map(|(_, source)| source)
    .collect();
    Some(Battery {
        level: if scale > 0 {
            percent(level, scale)
        } else {
            level
        },
        temperature_deci_c: temperature,
        voltage_mv: int("voltage"),
        status: match field("status") {
            Some("2") => BatteryStatus::Charging,
            Some("3") => BatteryStatus::Discharging,
            Some("4") => BatteryStatus::NotCharging,
            Some("5") => BatteryStatus::Full,
            _ => BatteryStatus::Unknown,
        },
        health: match field("health") {
            Some("2") => BatteryHealth::Good,
            Some("3") => BatteryHealth::Overheat,
            Some("4") => BatteryHealth::Dead,
            Some("5") => BatteryHealth::OverVoltage,
            Some("6") => BatteryHealth::Failure,
            Some("7") => BatteryHealth::Cold,
            _ => BatteryHealth::Unknown,
        },
        plugged,
    })
}

/// `Math.round(level * 100 / scale)`: the percentage, rounded half up, saturating at the ends of
/// `i64`. `scale` is positive.
fn percent(level: i64, scale: i64) -> i64 {
    let numerator = i128::from(level) * 100;
    let scale = i128::from(scale);
    let rounded = (2 * numerator + scale).div_euclid(2 * scale);
    i64::try_from(rounded).unwrap_or(if rounded < 0 { i64::MIN } else { i64::MAX })
}

/// One `  Key: value` line, as `/^\s+([A-Za-z][A-Za-z ]*):\s*(.*)$/` reads it.
struct BatteryField {
    /// The key, trimmed and in lower case.
    key: String,
    /// The value, trimmed.
    value: String,
}

impl BatteryField {
    fn read(line: &str) -> Option<Self> {
        let rest = text::trim_start(line);
        if rest.len() == line.len() || !rest.starts_with(|c: char| c.is_ascii_alphabetic()) {
            return None;
        }
        let key_end = rest
            .find(|c: char| !(c.is_ascii_alphabetic() || c == ' '))
            .unwrap_or(rest.len());
        let after = rest[key_end..].strip_prefix(':')?;
        let value = text::trim_start(after);
        if value.contains(text::is_line_terminator) {
            return None;
        }
        Some(Self {
            key: text::trim(&rest[..key_end]).to_ascii_lowercase(),
            value: text::trim(value).to_owned(),
        })
    }
}

/// Writes tenths of a degree as degrees, the way JavaScript prints `deciC / 10`: a whole number
/// without a fraction, such as `22`, and otherwise the shortest decimal, such as `22.1`.
#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's serialize_with passes the field by reference"
)]
#[expect(
    clippy::cast_precision_loss,
    reason = "battery temperatures are far below 2^53 tenths, where f64 holds every integer"
)]
fn serialize_celsius<S: Serializer>(deci_c: &i64, serializer: S) -> Result<S::Ok, S::Error> {
    if deci_c % 10 == 0 {
        serializer.serialize_i64(deci_c / 10)
    } else {
        serializer.serialize_f64(*deci_c as f64 / 10.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn battery(level: &str, scale: &str) -> Option<Battery> {
        parse_battery(&format!(
            "Current Battery Service state:\n  level: {level}\n  scale: {scale}\n  temperature: 300\n"
        ))
    }

    #[test]
    fn level_is_a_percentage_of_the_scale_rounded_half_up() {
        assert_eq!(battery("50", "200").map(|b| b.level), Some(25));
        assert_eq!(battery("1", "200").map(|b| b.level), Some(1));
        assert_eq!(battery("1", "3").map(|b| b.level), Some(33));
        assert_eq!(battery("2", "3").map(|b| b.level), Some(67));
        assert_eq!(battery("-1", "200").map(|b| b.level), Some(0));
        assert_eq!(battery("-3", "200").map(|b| b.level), Some(-1));
    }

    #[test]
    fn a_missing_or_unusable_scale() {
        let without = parse_battery("  level: 42\n  temperature: 300\n");
        assert_eq!(without.map(|b| b.level), Some(42));
        assert_eq!(battery("42", "0").map(|b| b.level), Some(42));
        assert_eq!(battery("42", "-5").map(|b| b.level), Some(42));
        assert_eq!(battery("42", "x").map(|b| b.level), Some(42));
    }

    #[test]
    fn level_and_temperature_are_required() {
        assert_eq!(parse_battery("  level: 42\n"), None);
        assert_eq!(parse_battery("  temperature: 42\n"), None);
        assert_eq!(parse_battery("  level: 4.2\n  temperature: 300\n"), None);
        assert_eq!(parse_battery("Can't find service: battery"), None);
    }

    #[test]
    fn lines_need_leading_space_and_a_letter_key() {
        assert_eq!(parse_battery("level: 42\ntemperature: 300\n"), None);
        assert_eq!(parse_battery("  level2: 42\n  temperature: 300\n"), None);
        assert_eq!(parse_battery("  level: 42\r9\n  temperature: 300\n"), None);
    }

    #[test]
    fn keys_ignore_case_and_the_last_value_wins() {
        let parsed = parse_battery("  LEVEL: 10\n  Temperature : 300 \n  level: 20\n");
        assert_eq!(
            parsed.map(|b| (b.level, b.temperature_deci_c)),
            Some((20, 300))
        );
    }

    #[test]
    fn unknown_codes_are_unknown() {
        let parsed = parse_battery("  level: 1\n  temperature: 1\n  status: 9\n  health: 02\n")
            .expect("a battery");
        assert_eq!(parsed.status, BatteryStatus::Unknown);
        assert_eq!(parsed.health, BatteryHealth::Unknown);
        assert_eq!(parsed.voltage_mv, None);
        assert_eq!(parsed.plugged, []);
    }

    #[test]
    fn serializes_as_the_api_battery() {
        let mut parsed = parse_battery(
            "  AC powered: true\n  Dock powered: true\n  USB powered: false\n  status: 2\n  \
             level: 85\n  temperature: 221\n  voltage: 4103\n  health: 3\n",
        )
        .expect("a battery");
        assert_eq!(
            serde_json::to_string(&parsed).expect("serializes"),
            r#"{"level":85,"temperatureC":22.1,"status":"charging","plugged":["ac","dock"]}"#
        );
        parsed.temperature_deci_c = 220;
        parsed.status = BatteryStatus::NotCharging;
        assert_eq!(
            serde_json::to_string(&parsed).expect("serializes"),
            r#"{"level":85,"temperatureC":22,"status":"not_charging","plugged":["ac","dock"]}"#
        );
        parsed.temperature_deci_c = -5;
        assert!(
            serde_json::to_string(&parsed)
                .expect("serializes")
                .contains(r#""temperatureC":-0.5"#),
            "a negative temperature keeps its sign"
        );
    }
}
