//! One phone rule: what it checks, how it reads, and the commands that make it pass.

use std::iter;

use crate::monitor::lock_screen_notice::{
    LOCK_SCREEN_DEX_PHONE_PATH, LOCK_SCREEN_DEX_SHA256, LockScreenNotice,
};
use crate::monitor::lock_wallpaper::LockWallpaper;
use crate::monitor::namespace::Namespace;
use crate::monitor::profile::ScreenSettings;
use crate::monitor::severity::Severity;
use crate::monitor::snapshot::{ActiveSub, Snapshot};

/// The role `sms_role` checks.
pub const SMS_ROLE: &str = "android.app.role.SMS";

/// What a check may depend on beside the read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CheckContext {
    /// A screen session streams from the phone, and scrcpy holds `stay_on_while_plugged_in`
    /// while it does.
    pub screen_session: bool,
}

/// A rule's verdict on one read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    /// The rule passes.
    pub ok: bool,
    /// What the read showed, such as `0`, `unset` or `mobile_data=0, mobile_data1=unset`.
    pub observed: String,
    /// Words that replace the rule's own instruction for this outcome.
    pub instruction: Option<&'static str>,
}

impl Outcome {
    fn new(ok: bool, observed: String) -> Self {
        Self {
            ok,
            observed,
            instruction: None,
        }
    }

    fn pass(observed: String) -> Self {
        Self {
            ok: true,
            observed,
            instruction: None,
        }
    }

    fn fail(observed: String) -> Self {
        Self {
            ok: false,
            observed,
            instruction: None,
        }
    }
}

/// A file Set up phone pushes before it runs a rule's commands. The server holds the bytes and
/// checks them against `sha256` when it loads them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PhoneFile {
    /// Where it goes on the phone, pushed with mode 0644.
    pub path: &'static str,
    /// The SHA-256 the bytes must have.
    pub sha256: &'static str,
}

/// A setting a rule reads, as an alternative of the read's `grep -E` pattern. Per-SIM keys carry
/// a pattern for the subscription id suffix, such as `mobile_data[0-9]*`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingKey {
    /// The table.
    pub namespace: Namespace,
    /// The key, or a pattern for the keys.
    pub key: String,
}

/// Whether a rule's commands may run on a check, or only on Set up phone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuleSelection {
    /// Every enforceable rule, for Set up phone.
    All,
    /// Only the rules the drift checker fixes on sight.
    AutoFix,
}

/// One rule of a policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    /// The rule's id, such as `mobile_data`.
    pub id: &'static str,
    /// What the rule wants, in words, such as `Mobile data off, on every SIM`.
    pub describe: String,
    /// What it wants, as the read shows it, such as `global wifi_on = 0`.
    pub want: String,
    /// How urgent a failure is.
    pub severity: Severity,
    /// The drift checker sets it on sight: the radio and data rules, and the log buffer. Other
    /// failing rules raise an alert and wait for Set up phone.
    pub auto_fix: bool,
    /// The rule keeps the phone off mobile data and radios. One still failing after the
    /// automatic fix is a critical alert.
    pub safety: bool,
    /// The event an automatic fix logs when the fix is routine, instead of `drift`.
    pub fix_event: Option<&'static str>,
    /// The settings it reads.
    pub settings: Vec<SettingKey>,
    /// Files its commands need on the phone.
    pub files: Vec<PhoneFile>,
    /// What the owner does on the phone when adb cannot set it, or what happens next when the
    /// automatic fix did not take.
    pub instruction: Option<&'static str>,
    kind: RuleKind,
}

/// Whether a per-SIM rule also checks telephony's own mobile data flag, in case a setting and
/// the data stack disagree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Telephony {
    DataEnabled,
    Ignored,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum RuleKind {
    Setting {
        namespace: Namespace,
        key: &'static str,
        want: String,
        command: Option<Vec<String>>,
        accept_during_screen_session: bool,
    },
    PerSimOff {
        key: &'static str,
        first: Vec<&'static str>,
        telephony: Telephony,
    },
    DataSaver,
    SmsRole {
        app: String,
    },
    Debloat,
    Mms,
    LogBuffer {
        mib: u8,
    },
    LockScreenNotice {
        name: String,
        command: String,
    },
}

/// Names shown for SMS apps a profile commonly picks.
fn app_name(app: &str) -> &str {
    match app {
        "com.google.android.apps.messaging" => "Google Messages",
        _ => app,
    }
}

/// `unset` for a setting the read didn't list.
fn show(value: Option<&str>) -> &str {
    value.unwrap_or("unset")
}

/// The command that sets the radio log buffers to `mib` MiB until the next reboot.
#[must_use]
pub fn log_buffer_command(mib: u8) -> String {
    format!("logcat -b main,system,radio,crash -G {mib}M")
}

/// The size in the largest unit that divides it, such as `256 KiB` or `8 MiB`.
fn format_bytes(bytes: u64) -> String {
    for (unit, size) in [
        ("GiB", 1_u64 << 30),
        ("MiB", 1 << 20),
        ("KiB", 1 << 10),
        ("B", 1),
    ] {
        if bytes >= size && bytes.is_multiple_of(size) {
            return format!("{} {unit}", bytes / size);
        }
    }
    format!("{bytes} B")
}

/// Settings rules share this shape.
struct SettingRule {
    id: &'static str,
    describe: String,
    namespace: Namespace,
    key: &'static str,
    want: String,
    severity: Severity,
    auto_fix: bool,
    safety: bool,
    command: Option<Vec<String>>,
    accept_during_screen_session: bool,
}

impl SettingRule {
    fn safety(id: &'static str, describe: &str, namespace: Namespace, key: &'static str) -> Self {
        Self {
            id,
            describe: describe.to_owned(),
            namespace,
            key,
            want: "0".to_owned(),
            severity: Severity::Critical,
            auto_fix: true,
            safety: true,
            command: None,
            accept_during_screen_session: false,
        }
    }

    fn manual(
        id: &'static str,
        describe: String,
        namespace: Namespace,
        key: &'static str,
        want: String,
        severity: Severity,
    ) -> Self {
        Self {
            id,
            describe,
            namespace,
            key,
            want,
            severity,
            auto_fix: false,
            safety: false,
            command: None,
            accept_during_screen_session: false,
        }
    }

    fn command(self, commands: &[&str]) -> Self {
        Self {
            command: Some(commands.iter().map(|&command| command.to_owned()).collect()),
            ..self
        }
    }

    fn rule(self) -> Rule {
        Rule {
            id: self.id,
            describe: self.describe,
            want: format!("{} {} = {}", self.namespace.as_str(), self.key, self.want),
            severity: self.severity,
            auto_fix: self.auto_fix,
            safety: self.safety,
            fix_event: None,
            settings: vec![SettingKey {
                namespace: self.namespace,
                key: self.key.to_owned(),
            }],
            files: Vec::new(),
            instruction: None,
            kind: RuleKind::Setting {
                namespace: self.namespace,
                key: self.key,
                want: self.want,
                command: self.command,
                accept_during_screen_session: self.accept_during_screen_session,
            },
        }
    }
}

impl Rule {
    fn per_sim_off(
        id: &'static str,
        describe: &str,
        key: &'static str,
        first: Vec<&'static str>,
        telephony: Telephony,
    ) -> Self {
        Self {
            id,
            describe: describe.to_owned(),
            want: format!("global {key} and {key}<subId> = 0"),
            severity: Severity::Critical,
            auto_fix: true,
            safety: true,
            fix_event: None,
            settings: vec![SettingKey {
                namespace: Namespace::Global,
                key: format!("{key}[0-9]*"),
            }],
            files: Vec::new(),
            instruction: None,
            kind: RuleKind::PerSimOff {
                key,
                first,
                telephony,
            },
        }
    }

    /// The radio and data rules, in check order.
    pub(crate) fn radio_rules() -> Vec<Self> {
        vec![
            Self::per_sim_off(
                "mobile_data",
                "Mobile data off, on every SIM",
                "mobile_data",
                vec!["svc data disable"],
                Telephony::DataEnabled,
            ),
            Self::per_sim_off(
                "data_roaming",
                "Data roaming off, on every SIM",
                "data_roaming",
                Vec::new(),
                Telephony::Ignored,
            ),
            SettingRule::safety("wifi_off", "Wi-Fi off", Namespace::Global, "wifi_on")
                .command(&["svc wifi disable"])
                .rule(),
            SettingRule::safety(
                "wifi_wakeup_off",
                "Wi-Fi does not turn itself back on",
                Namespace::Global,
                "wifi_wakeup_enabled",
            )
            .rule(),
            SettingRule::safety(
                "wifi_scan_off",
                "Wi-Fi scanning off",
                Namespace::Global,
                "wifi_scan_always_enabled",
            )
            .rule(),
            SettingRule::safety(
                "ble_scan_off",
                "Bluetooth scanning off",
                Namespace::Global,
                "ble_scan_always_enabled",
            )
            .rule(),
            SettingRule::safety(
                "bluetooth_off",
                "Bluetooth off",
                Namespace::Global,
                "bluetooth_on",
            )
            .command(&[
                "cmd bluetooth_manager disable",
                "cmd bluetooth_manager wait-for-state:STATE_OFF",
            ])
            .rule(),
            SettingRule::safety(
                "location_off",
                "Location off",
                Namespace::Secure,
                "location_mode",
            )
            .rule(),
        ]
    }

    /// `ota_off`.
    pub(crate) fn ota_off() -> Self {
        SettingRule::manual(
            "ota_off",
            "Automatic system updates off".to_owned(),
            Namespace::Global,
            "ota_disable_automatic_update",
            "1".to_owned(),
            Severity::Warning,
        )
        .rule()
    }

    /// The screen rules the profile's screen settings turn on, in check order.
    pub(crate) fn screen_rules(screen: &ScreenSettings) -> Vec<Self> {
        let mut rules = Vec::new();
        if let Some(stay_on) = screen.stay_on_while_charging {
            rules.push(
                SettingRule {
                    accept_during_screen_session: true,
                    ..SettingRule::manual(
                        "stay_on_off",
                        if stay_on {
                            "Screen stays on while charging"
                        } else {
                            "Screen does not stay on while charging"
                        }
                        .to_owned(),
                        Namespace::Global,
                        "stay_on_while_plugged_in",
                        if stay_on { "7" } else { "0" }.to_owned(),
                        Severity::Info,
                    )
                }
                .rule(),
            );
        }
        if let Some(automatic) = screen.auto_brightness {
            rules.push(
                SettingRule::manual(
                    "brightness_manual",
                    if automatic {
                        "Automatic brightness"
                    } else {
                        "Manual brightness"
                    }
                    .to_owned(),
                    Namespace::System,
                    "screen_brightness_mode",
                    if automatic { "1" } else { "0" }.to_owned(),
                    Severity::Info,
                )
                .rule(),
            );
        }
        if let Some(brightness) = screen.brightness {
            rules.push(
                SettingRule::manual(
                    "brightness_low",
                    format!("Brightness {brightness} of 255"),
                    Namespace::System,
                    "screen_brightness",
                    brightness.to_string(),
                    Severity::Info,
                )
                .rule(),
            );
        }
        if let Some(seconds) = screen.timeout_seconds {
            rules.push(
                SettingRule::manual(
                    "screen_timeout",
                    format!("Screen turns off after {seconds} s"),
                    Namespace::System,
                    "screen_off_timeout",
                    (u64::from(seconds) * 1000).to_string(),
                    Severity::Info,
                )
                .rule(),
            );
        }
        rules
    }

    fn plain(
        id: &'static str,
        describe: String,
        want: String,
        severity: Severity,
        kind: RuleKind,
    ) -> Self {
        Self {
            id,
            describe,
            want,
            severity,
            auto_fix: false,
            safety: false,
            fix_event: None,
            settings: Vec::new(),
            files: Vec::new(),
            instruction: None,
            kind,
        }
    }

    /// `data_saver`.
    pub(crate) fn data_saver() -> Self {
        Self::plain(
            "data_saver",
            "Data Saver on".to_owned(),
            "restrict-background enabled".to_owned(),
            Severity::Warning,
            RuleKind::DataSaver,
        )
    }

    /// `sms_role`. The SMS app is deliberately not put on the Data Saver allowlist: Messages
    /// downloads MMS through the platform `MmsService` in the phone process, a system uid Data
    /// Saver does not restrict (inferred), and allowlisting would only let it use data if data
    /// came on.
    pub(crate) fn sms_role(app: &str) -> Self {
        Self::plain(
            "sms_role",
            format!("{} is the default SMS app", app_name(app)),
            app.to_owned(),
            Severity::Critical,
            RuleKind::SmsRole {
                app: app.to_owned(),
            },
        )
    }

    /// `debloat`, for the profile's `count` listed apps.
    pub(crate) fn debloat(count: usize) -> Self {
        Self::plain(
            "debloat",
            "Unneeded apps removed".to_owned(),
            format!("none of the {count} listed apps installed for user 0"),
            Severity::Warning,
            RuleKind::Debloat,
        )
    }

    /// `mms_with_data_off`.
    pub(crate) fn mms() -> Self {
        Self {
            instruction: Some(
                "On the phone: Settings > Network & internet > SIMs > (the SIM) > turn on MMS \
                 messages.",
            ),
            ..Self::plain(
                "mms_with_data_off",
                "MMS works with mobile data off, on every SIM".to_owned(),
                "enabledMobileDataPolicies includes 2 (MMS_ALWAYS_ALLOWED)".to_owned(),
                Severity::Critical,
                RuleKind::Mms,
            )
        }
    }

    /// `log_buffer`. `logcat -G` resizes the buffers at once until the next reboot, as measured on
    /// the B30, so the drift checker sets it after every boot; that is routine, so its fix logs
    /// `log_buffer_set` rather than drift.
    pub(crate) fn log_buffer(mib: u8) -> Self {
        Self {
            auto_fix: true,
            fix_event: Some("log_buffer_set"),
            instruction: Some(
                "The phone ignored the buffer size; it will be retried on the next check.",
            ),
            ..Self::plain(
                "log_buffer",
                format!("Radio log buffer at least {mib} MiB"),
                format!("logcat -g -b radio reports a ring buffer of {mib} MiB or more"),
                Severity::Info,
                RuleKind::LogBuffer { mib },
            )
        }
    }

    /// The lock screen rules: notifications hidden, and the notice drawn.
    pub(crate) fn lock_screen_rules(notice: &LockScreenNotice) -> Vec<Self> {
        let name = notice.name();
        vec![
            SettingRule::manual(
                "lock_screen_notifications_off",
                "Notifications hidden on the lock screen".to_owned(),
                Namespace::Secure,
                "lock_screen_show_notifications",
                "0".to_owned(),
                Severity::Warning,
            )
            .rule(),
            Self {
                files: vec![PhoneFile {
                    path: LOCK_SCREEN_DEX_PHONE_PATH,
                    sha256: LOCK_SCREEN_DEX_SHA256,
                }],
                ..Self::plain(
                    "lock_screen_notice",
                    format!("Lock screen shows \"{}\"", notice.title),
                    format!("lock screen wallpaper named {name}"),
                    Severity::Info,
                    RuleKind::LockScreenNotice {
                        command: notice.command(),
                        name,
                    },
                )
            },
        ]
    }

    /// Whether Set up phone can make the rule pass over adb.
    #[must_use]
    pub fn fixable(&self) -> bool {
        !matches!(self.kind, RuleKind::Mms)
    }

    /// The rule's verdict on a read.
    #[must_use]
    pub fn check(&self, snapshot: &Snapshot, context: CheckContext) -> Outcome {
        match &self.kind {
            RuleKind::Setting {
                namespace,
                key,
                want,
                accept_during_screen_session,
                ..
            } => {
                let value = snapshot.setting(*namespace, key);
                if *accept_during_screen_session && context.screen_session {
                    return Outcome::pass(format!("{} (screen session)", show(value)));
                }
                Outcome::new(value == Some(want.as_str()), show(value).to_owned())
            }
            RuleKind::PerSimOff { key, telephony, .. } => {
                per_sim_outcome(key, *telephony, snapshot)
            }
            RuleKind::DataSaver => Outcome::new(
                snapshot.data_saver == Some(true),
                match snapshot.data_saver {
                    None => "unreadable",
                    Some(true) => "enabled",
                    Some(false) => "disabled",
                }
                .to_owned(),
            ),
            RuleKind::SmsRole { app } => {
                let holders = snapshot.sms_role_holders.as_deref().unwrap_or_default();
                let observed = holders.join(", ");
                Outcome::new(
                    holders.len() == 1 && holders[0] == *app,
                    if observed.is_empty() {
                        "none".to_owned()
                    } else {
                        observed
                    },
                )
            }
            RuleKind::Debloat => Outcome::new(
                snapshot.bloat.is_empty(),
                if snapshot.bloat.is_empty() {
                    "none installed".to_owned()
                } else {
                    format!("installed: {}", snapshot.bloat.join(", "))
                },
            ),
            RuleKind::Mms => mms_outcome(snapshot),
            RuleKind::LogBuffer { mib } => match snapshot.radio_buffer_bytes {
                None => Outcome {
                    ok: false,
                    observed: "unreadable".to_owned(),
                    instruction: Some(
                        "Couldn't read the buffer size; it will be retried on the next check.",
                    ),
                },
                Some(bytes) => {
                    Outcome::new(bytes >= u64::from(*mib) * 1024 * 1024, format_bytes(bytes))
                }
            },
            RuleKind::LockScreenNotice { name, .. } => {
                notice_outcome(name, &snapshot.lock_wallpaper)
            }
        }
    }

    /// The commands that make the rule pass, which are idempotent, or `None` when adb cannot set
    /// it. An empty list means nothing to run now, such as `stay_on_off` during a screen session.
    #[must_use]
    pub fn enforce(&self, snapshot: &Snapshot, context: CheckContext) -> Option<Vec<String>> {
        Some(match &self.kind {
            RuleKind::Setting {
                namespace,
                key,
                want,
                command,
                accept_during_screen_session,
            } => {
                if *accept_during_screen_session && context.screen_session {
                    return Some(Vec::new());
                }
                command.clone().unwrap_or_else(|| {
                    vec![format!("settings put {} {key} {want}", namespace.as_str())]
                })
            }
            RuleKind::PerSimOff { key, first, .. } => first
                .iter()
                .map(|&command| command.to_owned())
                .chain(
                    per_sim_keys(key, snapshot).map(|key| format!("settings put global {key} 0")),
                )
                .collect(),
            RuleKind::DataSaver => vec!["cmd netpolicy set restrict-background true".to_owned()],
            RuleKind::SmsRole { app } => vec![format!("cmd role add-role-holder {SMS_ROLE} {app}")],
            RuleKind::Debloat => snapshot
                .bloat
                .iter()
                .map(|name| format!("pm uninstall --user 0 {name}"))
                .collect(),
            RuleKind::Mms => return None,
            RuleKind::LogBuffer { mib } => vec![log_buffer_command(*mib)],
            RuleKind::LockScreenNotice { command, .. } => vec![command.clone()],
        })
    }
}

/// The verdict of a per-SIM rule: every copy of the key for an active SIM is 0, and for mobile
/// data, telephony reports no slot with data enabled.
fn per_sim_outcome(key: &str, telephony: Telephony, snapshot: &Snapshot) -> Outcome {
    let mut ok = true;
    let mut observed = Vec::new();
    for key in per_sim_keys(key, snapshot) {
        let value = snapshot.setting(Namespace::Global, &key);
        ok &= value == Some("0");
        observed.push(format!("{key}={}", show(value)));
    }
    if telephony == Telephony::DataEnabled {
        for slot in &snapshot.data_enabled {
            if slot.enabled == Some(true) {
                ok = false;
                observed.push(format!("slot {} data enabled", slot.slot));
            }
        }
    }
    Outcome::new(ok, observed.join(", "))
}

/// The verdict of `mms_with_data_off`: every active SIM keeps MMS with data off.
fn mms_outcome(snapshot: &Snapshot) -> Outcome {
    if snapshot.subs.is_empty() {
        return Outcome::pass("no SIM".to_owned());
    }
    let observed: Vec<String> = snapshot
        .subs
        .iter()
        .map(|sub| {
            let state = if sub.mms_with_data_off() { "on" } else { "off" };
            format!("SIM {} (sub {}) {state}", sub.slot + 1, sub.sub_id)
        })
        .collect();
    Outcome::new(
        snapshot.subs.iter().all(ActiveSub::mms_with_data_off),
        observed.join(", "),
    )
}

/// The verdict of `lock_screen_notice`: user 0's lock screen shows its own image wallpaper under
/// the notice's name.
fn notice_outcome(name: &str, wallpaper: &LockWallpaper) -> Outcome {
    match wallpaper {
        LockWallpaper::Own { name: shown } => Outcome::new(
            shown == name,
            if shown.is_empty() {
                "an unnamed wallpaper".to_owned()
            } else {
                shown.clone()
            },
        ),
        LockWallpaper::Live { component } => {
            Outcome::fail(format!("a live wallpaper ({component})"))
        }
        LockWallpaper::Shared => Outcome::fail("the home screen wallpaper".to_owned()),
        LockWallpaper::Unreadable => Outcome::fail("unreadable".to_owned()),
    }
}

/// A per-SIM key and its copy for every active SIM: `mobile_data`, `mobile_data1`, ...
fn per_sim_keys<'s>(key: &'s str, snapshot: &'s Snapshot) -> impl Iterator<Item = String> + 's {
    iter::once(key.to_owned()).chain(
        snapshot
            .subs
            .iter()
            .map(move |sub| format!("{key}{}", sub.sub_id)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_use_the_largest_unit_that_divides_them() {
        assert_eq!(format_bytes(256 * 1024), "256 KiB");
        assert_eq!(format_bytes(8 << 20), "8 MiB");
        assert_eq!(format_bytes(8191 * 1024), "8191 KiB");
        assert_eq!(format_bytes(1 << 30), "1 GiB");
        assert_eq!(format_bytes(1025), "1025 B");
        assert_eq!(format_bytes(0), "0 B");
    }
}
