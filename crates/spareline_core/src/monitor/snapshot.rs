//! What one policy read returned, parsed.

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;

use crate::monitor::js_regex;
use crate::monitor::lock_wallpaper::LockWallpaper;
use crate::monitor::namespace::Namespace;
use crate::{android, text};

/// Telephony's `MMS_ALWAYS_ALLOWED` data policy: "MMS messages" stays on with mobile data off.
pub const MMS_ALWAYS_ALLOWED: i64 = 2;

static SUB_ID: LazyLock<Regex> =
    LazyLock::new(|| js_regex::pattern(r"SubscriptionInfoInternal: id=([0-9]+) "));
static DATA_SAVER: LazyLock<Regex> =
    LazyLock::new(|| js_regex::pattern(r"status:{s}*(enabled|disabled)"));
static DATA_POLICIES: LazyLock<Regex> =
    LazyLock::new(|| js_regex::pattern(r"enabledMobileDataPolicies=([0-9,]*)"));
static ROLE_SEPARATOR: LazyLock<Regex> = LazyLock::new(|| js_regex::pattern(r"(?:{s}|;)+"));
static RING_BUFFER: LazyLock<Regex> =
    LazyLock::new(|| js_regex::pattern(r"ring buffer is ([0-9.]+) ?(B|KiB|MiB|GiB){b}"));

/// One SIM that isub lists as active.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveSub {
    /// The subscription id.
    pub sub_id: i64,
    /// The logical slot.
    pub slot: u32,
    /// Telephony's `enabledMobileDataPolicies`, or `None` when isub doesn't print the field.
    pub data_policies: Option<Vec<i64>>,
}

impl ActiveSub {
    /// Whether "MMS messages" stays on with mobile data off for this SIM.
    #[must_use]
    pub fn mms_with_data_off(&self) -> bool {
        self.data_policies
            .as_ref()
            .is_some_and(|policies| policies.contains(&MMS_ALWAYS_ALLOWED))
    }
}

/// Telephony's own view of mobile data in one slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlotDataEnabled {
    /// The logical slot.
    pub slot: u32,
    /// `mIsDataEnabled`, when it reads `true` or `false`.
    pub enabled: Option<bool>,
}

/// What one read returned, parsed. A section the read didn't print parses as empty.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    /// The settings the read listed, by table. A key the read didn't list is not set, which for
    /// the per-SIM data and roaming keys is not off: with `ro.com.android.mobiledata` unset, as
    /// measured on the B30, a new SIM's data defaults to on.
    pub settings: HashMap<Namespace, HashMap<String, String>>,
    /// The active SIMs.
    pub subs: Vec<ActiveSub>,
    /// Every subscription id isub lists, SIMs no longer inserted included.
    pub known_sub_ids: Vec<i64>,
    /// Telephony's own view of mobile data, by slot.
    pub data_enabled: Vec<SlotDataEnabled>,
    /// Whether Data Saver is on, when the read says.
    pub data_saver: Option<bool>,
    /// The SMS role's holders, or `None` when the read had no such section.
    pub sms_role_holders: Option<Vec<String>>,
    /// Apps the profile removes that are still installed for user 0.
    pub bloat: Vec<String>,
    /// The radio log buffer's size in effect, not the `persist.logd.size` setting: on the B30 the
    /// setting said 8M while the buffer was 256 KiB.
    pub radio_buffer_bytes: Option<u64>,
    /// The phone's clock in seconds since the epoch, which netstats buckets use.
    pub phone_clock: Option<i64>,
    /// The raw `dumpsys netstats --full --uid` output, for [`crate::monitor::parse_netstats`].
    pub netstats: String,
    /// User 0's lock screen wallpaper.
    pub lock_wallpaper: LockWallpaper,
}

impl Snapshot {
    /// Parses a policy read. Only the names in `remove_apps` count as bloat, so nothing else
    /// can reach an uninstall command.
    #[must_use]
    pub fn parse(stdout: &str, remove_apps: &[String]) -> Self {
        let sections = android::split_sections(stdout);
        let get = |name: &str| sections.get(name).unwrap_or_default();
        let mut settings = HashMap::new();
        for namespace in Namespace::ALL {
            let mut values = HashMap::new();
            for line in text::lines(get(&format!("settings:{}", namespace.as_str()))) {
                if let Some(eq) = line.find('=')
                    && eq > 0
                {
                    values.insert(
                        line[..eq].to_owned(),
                        text::trim(&line[eq + 1..]).to_owned(),
                    );
                }
            }
            settings.insert(namespace, values);
        }
        let isub = get("isub");
        let subs: Vec<ActiveSub> = android::parse_isub(isub)
            .active
            .into_iter()
            .map(|sub| ActiveSub {
                sub_id: sub.sub_id,
                slot: sub.slot,
                data_policies: data_policies(isub, sub.sub_id),
            })
            .collect();
        let clock = get("clock");
        let roles = get("smsrole");
        Self {
            settings,
            subs,
            known_sub_ids: SUB_ID
                .captures_iter(isub)
                .map(|id| id[1].parse().unwrap_or(i64::MAX))
                .collect(),
            data_enabled: android::parse_registry(get("registry"))
                .into_iter()
                .map(|radio| SlotDataEnabled {
                    slot: radio.slot,
                    enabled: radio.data_enabled,
                })
                .collect(),
            data_saver: DATA_SAVER
                .captures(get("datasaver"))
                .map(|status| &status[1] == "enabled"),
            sms_role_holders: sections.get("smsrole").map(|_| {
                ROLE_SEPARATOR
                    .split(roles)
                    .filter(|holder| !holder.is_empty())
                    .map(str::to_owned)
                    .collect()
            }),
            bloat: text::lines(get("bloat"))
                .map(|line| {
                    let line = text::trim(line);
                    line.strip_prefix("package:").unwrap_or(line)
                })
                .filter(|name| remove_apps.iter().any(|app| app == name))
                .map(str::to_owned)
                .collect(),
            radio_buffer_bytes: buffer_bytes(get("radiobuf")),
            phone_clock: (!clock.is_empty() && clock.bytes().all(|byte| byte.is_ascii_digit()))
                .then(|| clock.parse().ok())
                .flatten(),
            netstats: get("netstats").to_owned(),
            lock_wallpaper: LockWallpaper::parse(get("wallpaper")),
        }
    }

    /// The value of a setting, or `None` when the read didn't list it.
    #[must_use]
    pub fn setting(&self, namespace: Namespace, key: &str) -> Option<&str> {
        self.settings
            .get(&namespace)
            .and_then(|values| values.get(key))
            .map(String::as_str)
    }
}

/// The `enabledMobileDataPolicies` on the first isub line about `sub_id`.
fn data_policies(isub: &str, sub_id: i64) -> Option<Vec<i64>> {
    let marker = format!("SubscriptionInfoInternal: id={sub_id} ");
    let line = text::lines(isub).find(|line| line.contains(&marker))?;
    let policies = DATA_POLICIES.captures(line)?;
    Some(
        policies[1]
            .split(',')
            .filter(|policy| !policy.is_empty())
            .map(|policy| policy.parse().unwrap_or(i64::MAX))
            .collect(),
    )
}

/// The size `logcat -g -b radio` reports in effect: "radio: ring buffer is 256 KiB (252 KiB
/// consumed, 1 MiB readable), max entry is 5120 B, ...". A size that isn't a number, such as
/// `1.2.3`, reads as `None`.
fn buffer_bytes(text: &str) -> Option<u64> {
    let found = RING_BUFFER.captures(text)?;
    let unit: f64 = match &found[2] {
        "KiB" => 1024.0,
        "MiB" => 1024.0 * 1024.0,
        "GiB" => 1024.0 * 1024.0 * 1024.0,
        _ => 1.0,
    };
    let size: f64 = found[1].parse().ok()?;
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the size is a non-negative finite number of bytes, rounded as Math.round does"
    )]
    Some((size * unit + 0.5).floor() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECORDED: &str = include_str!("../../tests/fixtures/nuu-s6702x/policy-read.txt");

    #[test]
    fn the_recorded_read_parses() {
        let snapshot = Snapshot::parse(RECORDED, &[]);
        assert_eq!(
            snapshot.subs,
            [ActiveSub {
                sub_id: 1,
                slot: 0,
                data_policies: Some(vec![2])
            }]
        );
        assert!(snapshot.subs[0].mms_with_data_off());
        assert_eq!(snapshot.known_sub_ids, [1, 1, 1]);
        assert_eq!(snapshot.radio_buffer_bytes, Some(256 * 1024));
        assert!(snapshot.phone_clock.unwrap() > 1_790_000_000);
        assert_eq!(snapshot.data_saver, Some(true));
        assert_eq!(
            snapshot.sms_role_holders,
            Some(vec!["com.google.android.apps.messaging".to_owned()])
        );
        assert_eq!(snapshot.bloat, Vec::<String>::new());
        assert_eq!(
            snapshot.setting(Namespace::Global, "mobile_data1"),
            Some("0")
        );
        assert_eq!(
            snapshot.setting(Namespace::Secure, "location_mode"),
            Some("0")
        );
        assert_eq!(
            snapshot.setting(Namespace::System, "screen_brightness"),
            Some("10")
        );
        assert_eq!(snapshot.setting(Namespace::Global, "mobile_data2"), None);
        assert_eq!(
            snapshot.data_enabled,
            [
                SlotDataEnabled {
                    slot: 0,
                    enabled: Some(false)
                },
                SlotDataEnabled {
                    slot: 1,
                    enabled: Some(false)
                }
            ]
        );
        assert!(snapshot.netstats.starts_with("Directory:"));
    }

    #[test]
    fn an_empty_read_has_nothing_in_it() {
        let snapshot = Snapshot::parse("", &[]);
        assert_eq!(snapshot.subs, []);
        assert_eq!(snapshot.sms_role_holders, None);
        assert_eq!(snapshot.data_saver, None);
        assert_eq!(snapshot.radio_buffer_bytes, None);
        assert_eq!(snapshot.phone_clock, None);
        assert_eq!(snapshot.lock_wallpaper, LockWallpaper::Unreadable);
    }

    #[test]
    fn role_holders_split_on_white_space_and_semicolons() {
        let snapshot = Snapshot::parse("@@sl:smsrole\na.b; c.d  e.f;;\n", &[]);
        assert_eq!(
            snapshot.sms_role_holders,
            Some(vec!["a.b".to_owned(), "c.d".to_owned(), "e.f".to_owned()])
        );
        assert_eq!(
            Snapshot::parse("@@sl:smsrole\n\n", &[]).sms_role_holders,
            Some(Vec::new())
        );
    }

    #[test]
    fn buffer_sizes_round_as_math_round_does() {
        assert_eq!(buffer_bytes("ring buffer is 8 MiB"), Some(8 << 20));
        assert_eq!(buffer_bytes("ring buffer is 1.5 KiB"), Some(1536));
        assert_eq!(buffer_bytes("ring buffer is 0.0005 KiB"), Some(1));
        assert_eq!(buffer_bytes("ring buffer is 100B"), Some(100));
        assert_eq!(buffer_bytes("ring buffer is 8 MiBs"), None);
        assert_eq!(buffer_bytes("ring buffer is 1.2.3 KiB"), None);
        assert_eq!(buffer_bytes("logcat: Unable to open log device"), None);
    }
}
