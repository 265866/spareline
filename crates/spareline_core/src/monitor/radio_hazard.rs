//! The radio guard: every command the monitor runs on a phone passes it first.

use std::sync::LazyLock;

use regex::Regex;

use crate::monitor::js_regex;

/// Commands that turn a radio or data on, write airplane mode, or turn Data Saver off, each with
/// the reason the guard gives, in [`js_regex::pattern`]'s notation.
const ENABLING: &[(&str, &str)] = &[
    (
        r"{b}svc{s}+(wifi|data|bluetooth|nfc){s}+enable{b}",
        "svc ... enable",
    ),
    (
        r"{b}cmd{s}+bluetooth_manager{s}+enable{b}",
        "bluetooth_manager enable",
    ),
    (
        r"{b}cmd{s}+wifi{s}+set-wifi-enabled{s}+enabled{b}",
        "cmd wifi enable",
    ),
    (
        r"{b}cmd{s}+phone{s}+data{s}+enable{b}",
        "cmd phone data enable",
    ),
    (
        r"{b}cmd{s}+location{s}+set-location-enabled{s}+true{b}",
        "location enable",
    ),
    (
        r"{b}airplane_mode_on{b}|{b}airplane-mode{b}",
        "airplane mode change",
    ),
    (r"{b}restrict-background{s}+false{b}", "Data Saver off"),
];

/// `settings put` of a radio or data key, whose value must be `0`.
const RADIO_KEYS: &str = r"{b}settings{s}+put{s}+(global|secure){s}+(mobile_data[0-9]*|data_roaming[0-9]*|wifi_on|wifi_wakeup_enabled|wifi_scan_always_enabled|ble_scan_always_enabled|bluetooth_on|location_mode){s}+({S}+)";

static ENABLING_PATTERNS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    ENABLING
        .iter()
        .map(|&(template, reason)| (js_regex::pattern(template), reason))
        .collect()
});

static RADIO_KEY_PATTERN: LazyLock<Regex> = LazyLock::new(|| js_regex::pattern(RADIO_KEYS));

/// Why `command` could turn a radio or data on, or `None` when it cannot. Every command the
/// monitor runs on a phone passes through this first, whatever the profile: enabling Wi-Fi, data,
/// Bluetooth, NFC or location, any airplane mode change, Data Saver off, and a `settings put` of a
/// radio or data key to anything but `0`, anywhere in the command.
#[must_use]
pub fn radio_hazard(command: &str) -> Option<String> {
    for (pattern, reason) in ENABLING_PATTERNS.iter() {
        if pattern.is_match(command) {
            return Some((*reason).to_owned());
        }
    }
    for put in RADIO_KEY_PATTERN.captures_iter(command) {
        if &put[3] != "0" {
            return Some(format!("{} set to {}", &put[2], &put[3]));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG_BUFFER_COMMAND: &str = "logcat -b main,system,radio,crash -G 8M";

    fn refused(command: &str) -> bool {
        radio_hazard(command).is_some()
    }

    #[test]
    fn the_radio_guard_refuses_anything_that_could_turn_a_radio_or_data_on() {
        for command in [
            "svc data enable",
            "svc wifi enable",
            "svc bluetooth enable",
            "cmd bluetooth_manager enable",
            "cmd wifi set-wifi-enabled enabled",
            "cmd phone data enable",
            "cmd location set-location-enabled true",
            "settings put global mobile_data 1",
            "settings put global mobile_data2 1",
            "settings put global data_roaming1 1",
            "settings put global wifi_on 1",
            "settings put global bluetooth_on 1",
            "settings put global wifi_scan_always_enabled 1",
            "settings put global ble_scan_always_enabled 1",
            "settings put global wifi_wakeup_enabled 1",
            "settings put secure location_mode 3",
            "settings put global airplane_mode_on 0",
            "cmd connectivity airplane-mode disable",
            "cmd netpolicy set restrict-background false",
            "settings put global mobile_data 0; settings put global wifi_on 1",
        ] {
            assert!(refused(command), "{command} passed the guard");
        }
        for command in [
            "svc data disable",
            "settings put global mobile_data1 0",
            "settings put secure location_mode 0",
            "cmd netpolicy set restrict-background true",
        ] {
            assert_eq!(radio_hazard(command), None, "{command} was refused");
        }
    }

    #[test]
    fn the_radio_guard_allows_the_log_buffer_command_but_nothing_enabling_beside_it() {
        assert_eq!(radio_hazard(LOG_BUFFER_COMMAND), None);
        assert_eq!(radio_hazard("logcat -b radio -G 8M"), None);
        for enabling in [
            "svc data enable",
            "svc wifi enable",
            "cmd bluetooth_manager enable",
            "settings put global mobile_data1 1",
            "settings put secure location_mode 3",
            "cmd connectivity airplane-mode disable",
        ] {
            let command = format!("{LOG_BUFFER_COMMAND}; {enabling}");
            assert!(refused(&command), "{command} passed the guard");
            assert!(refused(enabling), "{enabling} passed the guard");
        }
    }

    #[test]
    fn each_refusal_names_its_reason() {
        for (command, reason) in [
            ("svc nfc enable", "svc ... enable"),
            ("cmd bluetooth_manager enable", "bluetooth_manager enable"),
            ("cmd wifi set-wifi-enabled enabled", "cmd wifi enable"),
            ("cmd phone data enable", "cmd phone data enable"),
            ("cmd location set-location-enabled true", "location enable"),
            (
                "settings get global airplane_mode_on",
                "airplane mode change",
            ),
            (
                "cmd connectivity airplane-mode enable",
                "airplane mode change",
            ),
            (
                "cmd netpolicy set restrict-background false",
                "Data Saver off",
            ),
            (
                "settings put global mobile_data7 1",
                "mobile_data7 set to 1",
            ),
            (
                "settings put secure location_mode 3",
                "location_mode set to 3",
            ),
            ("settings put global wifi_on true", "wifi_on set to true"),
        ] {
            assert_eq!(radio_hazard(command).as_deref(), Some(reason), "{command}");
        }
    }

    #[test]
    fn enabling_patterns_come_before_settings_puts() {
        assert_eq!(
            radio_hazard("settings put global wifi_on 1; svc data enable").as_deref(),
            Some("svc ... enable")
        );
        assert_eq!(
            radio_hazard("settings put global mobile_data 0 && settings put global wifi_on 1")
                .as_deref(),
            Some("wifi_on set to 1"),
            "the first put that isn't 0 is the one named"
        );
        assert_eq!(
            radio_hazard("settings put global mobile_data 0; settings put global wifi_on 1")
                .as_deref(),
            Some("mobile_data set to 0;"),
            "a value runs to the next white space, so `0;` is not `0`, as in the original"
        );
    }

    #[test]
    fn only_the_exact_value_0_turns_a_radio_key_off() {
        for value in ["00", "-0", "0.0", "1", "\"0\"", "0x0", "false", "off"] {
            let command = format!("settings put global mobile_data {value}");
            assert_eq!(
                radio_hazard(&command),
                Some(format!("mobile_data set to {value}")),
                "{command}"
            );
        }
        assert_eq!(
            radio_hazard("settings put global mobile_data 0;"),
            Some("mobile_data set to 0;".to_owned()),
            "the value runs to the next white space, as \\S+ does"
        );
        assert_eq!(
            radio_hazard("settings put global mobile_data 0 ; true"),
            None
        );
    }

    #[test]
    fn any_white_space_javascript_knows_separates_the_words() {
        for space in [
            " ", "\t", "  ", "\n", "\r", "\u{b}", "\u{c}", "\u{a0}", "\u{2003}", "\u{3000}",
            "\u{feff}", "\u{2028}",
        ] {
            let svc = format!("svc{space}data{space}enable");
            assert!(refused(&svc), "{svc:?} passed the guard");
            let put = format!("settings{space}put{space}global{space}wifi_on{space}1");
            assert!(refused(&put), "{put:?} passed the guard");
        }
        assert!(
            !refused("svc\u{85}data\u{85}enable"),
            "JavaScript's \\s does not include U+0085, so neither does the guard"
        );
    }

    #[test]
    fn word_boundaries_follow_javascripts_ascii_rule() {
        for command in [
            "/system/bin/svc data enable",
            "(svc data enable)",
            "x;svc data enable",
            "ésvc data enable",
            "svc data enable;",
            "svc data enable&&true",
            "airplane-mode",
            "foo.airplane_mode_on",
        ] {
            assert!(refused(command), "{command} passed the guard");
        }
        for command in [
            "xsvc data enable",
            "svc data enabled",
            "svc data enable_now",
            "svc data enable2",
            "my_svc data enable",
            "cmd bluetooth_manager enabled",
            "settings put global wifi_onx 1",
            "settings put global mobile_datax 1",
            "settings put global mobile_data1x 1",
            "xsettings put global wifi_on 1",
            "airplane_mode_onx",
            "xairplane-mode",
            "restrict-background falsey",
        ] {
            assert_eq!(radio_hazard(command), None, "{command} was refused");
        }
    }

    #[test]
    fn the_guard_is_case_sensitive_like_the_original() {
        for command in [
            "SVC data enable",
            "svc DATA enable",
            "Settings put global wifi_on 1",
            "settings put GLOBAL wifi_on 1",
            "settings put global WIFI_ON 1",
            "AIRPLANE_MODE_ON",
        ] {
            assert_eq!(radio_hazard(command), None, "{command} was refused");
        }
    }

    #[test]
    fn other_namespaces_and_other_keys_pass() {
        for command in [
            "settings put system wifi_on 1",
            "settings put global stay_on_while_plugged_in 7",
            "settings put system screen_brightness 255",
            "settings put global ota_disable_automatic_update 1",
            "settings put secure lock_screen_show_notifications 0",
            "settings put global mobile_data_always_on 1",
            "settings get global wifi_on",
            "settings put global wifi_on",
            "svc data disable",
            "svc wifi disable",
            "cmd bluetooth_manager disable",
            "cmd bluetooth_manager wait-for-state:STATE_OFF",
            "cmd role add-role-holder android.app.role.SMS com.google.android.apps.messaging",
            "pm uninstall --user 0 com.android.chrome",
        ] {
            assert_eq!(radio_hazard(command), None, "{command} was refused");
        }
    }

    #[test]
    fn every_svc_radio_and_every_radio_key_is_covered() {
        for radio in ["wifi", "data", "bluetooth", "nfc"] {
            assert!(refused(&format!("svc {radio} enable")), "svc {radio}");
            assert_eq!(radio_hazard(&format!("svc {radio} disable")), None);
        }
        for key in [
            "mobile_data",
            "mobile_data12",
            "data_roaming",
            "data_roaming3",
            "wifi_on",
            "wifi_wakeup_enabled",
            "wifi_scan_always_enabled",
            "ble_scan_always_enabled",
            "bluetooth_on",
            "location_mode",
        ] {
            for namespace in ["global", "secure"] {
                assert_eq!(
                    radio_hazard(&format!("settings put {namespace} {key} 1")),
                    Some(format!("{key} set to 1"))
                );
                assert_eq!(
                    radio_hazard(&format!("settings put {namespace} {key} 0")),
                    None
                );
            }
        }
    }
}
