//! A policy profile: the JSON file that turns rule groups on.

use serde::Deserialize;

use crate::monitor::json_value::JsonValue;
use crate::monitor::lock_screen_notice::LockScreenNotice;
use crate::monitor::profile_schema;

/// A policy profile, such as `profiles/no-data.json`. Every group is optional; a profile turns on
/// only what it names. Strict at every level: an unknown key is an error.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Profile {
    /// Mobile data, roaming, Wi-Fi and its scanning and wake-up, Bluetooth and its scanning, and
    /// location stay off. It also turns data off when a SIM changes, writes the next SIMs' data
    /// keys ahead of time, and watches mobile data use.
    pub radios_off: Option<RadiosOff>,
    /// Data Saver stays on.
    #[serde(default)]
    pub data_saver: bool,
    /// The package that must be the default SMS app.
    pub sms_app: Option<String>,
    /// Apps Set up phone removes for user 0. `pm uninstall --user 0` deletes the app's data for
    /// user 0; `cmd package install-existing <pkg>` brings the app back without it.
    #[serde(default)]
    pub remove_apps: Vec<String>,
    /// Screen settings.
    #[serde(default)]
    pub screen: ScreenSettings,
    /// Automatic system updates stay off.
    #[serde(default)]
    pub disable_system_updates: bool,
    /// The radio log buffer the drift checker keeps at least this large, in MiB (`logcat -G`).
    /// Some phones ignore Developer options' "Logger buffer sizes" at boot: the NUU B30 measured
    /// 256 KiB after every reboot.
    #[serde(rename = "radioLogBufferMiB")]
    pub radio_log_buffer_mib: Option<u8>,
    /// "MMS messages" stays on with mobile data off, on every SIM. adb cannot set it, so it is
    /// only checked.
    #[serde(default)]
    pub mms_with_data_off: bool,
    /// A notice on the lock screen for whoever picks the phone up, drawn as the lock screen
    /// wallpaper by Set up phone. Notifications leave the lock screen too: they cover the notice
    /// and show texts without an unlock.
    pub lock_screen: Option<LockScreenNotice>,
}

/// The radios-off group's settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RadiosOff {
    /// Internet mobile bytes per SIM in 24 h that turn data off again and raise an alert.
    #[serde(default = "default_data_threshold_bytes")]
    pub data_threshold_bytes: u64,
}

const fn default_data_threshold_bytes() -> u64 {
    1_000_000
}

impl Default for RadiosOff {
    fn default() -> Self {
        Self {
            data_threshold_bytes: default_data_threshold_bytes(),
        }
    }
}

/// The screen group's settings; each one present adds its rule.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ScreenSettings {
    /// Automatic brightness on or off.
    pub auto_brightness: Option<bool>,
    /// The brightness, 0 to 255.
    pub brightness: Option<u8>,
    /// The screen timeout in seconds, 1 to 86 400.
    pub timeout_seconds: Option<u32>,
    /// Whether the screen stays on while charging.
    pub stay_on_while_charging: Option<bool>,
}

/// A profile that can't be used.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[must_use]
pub enum ProfileError {
    /// The file couldn't be read or isn't JSON.
    #[error("cannot read policy profile {path}: {reason}")]
    Unreadable {
        /// The path as given.
        path: String,
        /// Why, such as the I/O error or the JSON syntax error.
        reason: String,
    },
    /// The JSON doesn't match the schema.
    #[error("invalid policy profile {path}:\n{issues}")]
    Invalid {
        /// The path as given.
        path: String,
        /// Every problem, one `✖ <message>` line each, followed by `  → at <path>` unless the
        /// problem is with the whole profile, as spareline-old printed them.
        issues: String,
    },
}

impl ProfileError {
    /// The error for a profile file that couldn't be read, with the reason as text.
    pub fn unreadable(path: &str, reason: &str) -> Self {
        Self::Unreadable {
            path: path.to_owned(),
            reason: reason.to_owned(),
        }
    }
}

impl Profile {
    /// Reads a profile's JSON text. `path` names the file in errors.
    ///
    /// # Errors
    ///
    /// [`ProfileError::Unreadable`] when `text` isn't JSON, and [`ProfileError::Invalid`] with
    /// every problem named when it doesn't match the schema.
    pub fn from_json(path: &str, text: &str) -> Result<Self, ProfileError> {
        let value = JsonValue::parse(text)
            .map_err(|error| ProfileError::unreadable(path, &error.to_string()))?;
        let checked = profile_schema::validate(&value).map_err(|issues| ProfileError::Invalid {
            path: path.to_owned(),
            issues,
        })?;
        serde_json::from_value(checked.to_serde()).map_err(|error| ProfileError::Invalid {
            path: path.to_owned(),
            issues: format!("✖ {error}"),
        })
    }
}

/// The profile's name: the file's base name without `.json`, as Node's
/// `path.basename(path, ".json")` gives it, trailing slashes ignored.
#[must_use]
pub fn profile_name(path: &str) -> String {
    const EXTENSION: &str = ".json";
    if path == EXTENSION {
        return String::new();
    }
    let trimmed = path.trim_end_matches('/');
    let base = trimmed.rsplit('/').next().unwrap_or(trimmed);
    match base.strip_suffix(EXTENSION) {
        Some(stem) if !stem.is_empty() => stem.to_owned(),
        _ => base.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issues(json: &str) -> String {
        match Profile::from_json("test.json", json) {
            Err(ProfileError::Invalid { issues, .. }) => issues,
            other => panic!("expected an invalid profile, got {other:?}"),
        }
    }

    #[test]
    fn the_shipped_profiles_parse() {
        let no_data = Profile::from_json(
            "no-data.json",
            include_str!("../../tests/fixtures/no-data.json"),
        )
        .unwrap();
        assert_eq!(
            no_data,
            Profile {
                radios_off: Some(RadiosOff {
                    data_threshold_bytes: 1_000_000
                }),
                data_saver: true,
                disable_system_updates: true,
                mms_with_data_off: true,
                ..Profile::default()
            }
        );
        let b30 = Profile::from_json(
            "nuu-b30-no-data.json",
            include_str!("../../tests/fixtures/nuu-b30-no-data.json"),
        )
        .unwrap();
        assert_eq!(b30.remove_apps.len(), 37);
        assert_eq!(
            b30.sms_app.as_deref(),
            Some("com.google.android.apps.messaging")
        );
        assert_eq!(
            b30.screen,
            ScreenSettings {
                auto_brightness: Some(false),
                brightness: Some(10),
                timeout_seconds: Some(15),
                stay_on_while_charging: Some(false),
            }
        );
        assert_eq!(b30.radio_log_buffer_mib, Some(8));
        assert_eq!(b30.lock_screen, Some(LockScreenNotice::default()));
    }

    #[test]
    fn defaults_fill_what_a_profile_leaves_out() {
        assert_eq!(Profile::from_json("p", "{}").unwrap(), Profile::default());
        let profile = Profile::from_json(
            "p",
            r#"{"radiosOff":{},"lockScreen":{"title":"  hi  ","lines":[" a "]}}"#,
        )
        .unwrap();
        assert_eq!(profile.radios_off, Some(RadiosOff::default()));
        assert_eq!(
            profile.lock_screen,
            Some(LockScreenNotice {
                title: "hi".to_owned(),
                lines: vec!["a".to_owned()]
            })
        );
        assert_eq!(
            Profile::from_json("p", r#"{"lockScreen":{}}"#)
                .unwrap()
                .lock_screen,
            Some(LockScreenNotice::default())
        );
        assert_eq!(
            Profile::from_json("p", r#"{"radioLogBufferMiB":8.0}"#)
                .unwrap()
                .radio_log_buffer_mib,
            Some(8)
        );
    }

    #[test]
    fn a_bad_profile_is_refused_with_every_problem_named_as_zod_named_it() {
        let error = Profile::from_json(
            "/etc/test.json",
            r#"{"radiosOf":{},"removeApps":["not a package; rm -rf /"],"screen":{"brightness":300}}"#,
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "invalid policy profile /etc/test.json:\n\
             ✖ Unrecognized key: \"radiosOf\"\n\
             ✖ an Android package name\n  → at removeApps[0]\n\
             ✖ Too big: expected number to be <=255\n  → at screen.brightness"
        );
    }

    #[test]
    fn json_that_does_not_parse_is_unreadable() {
        let error = Profile::from_json("p.json", "{ not json").unwrap_err();
        assert!(matches!(error, ProfileError::Unreadable { .. }));
        assert!(
            error
                .to_string()
                .starts_with("cannot read policy profile p.json: "),
            "{error}"
        );
        assert_eq!(
            ProfileError::unreadable("p.json", "no such file").to_string(),
            "cannot read policy profile p.json: no such file"
        );
    }

    fn check_cases(cases: &[[&str; 2]]) {
        for [json, expected] in cases {
            assert_eq!(issues(json), *expected, "for {json}");
        }
    }

    /// The expected texts in these tests are what zod 4.6.7's `prettifyError` printed for
    /// spareline-old's schema.
    #[test]
    fn type_errors_read_as_zod_wrote_them() {
        check_cases(&[
            ["[]", "✖ Invalid input: expected object, received array"],
            ["null", "✖ Invalid input: expected object, received null"],
            ["\"x\"", "✖ Invalid input: expected object, received string"],
            ["5", "✖ Invalid input: expected object, received number"],
            ["true", "✖ Invalid input: expected object, received boolean"],
            [
                r#"{"radiosOff":null}"#,
                "✖ Invalid input: expected object, received null\n  → at radiosOff",
            ],
            [
                r#"{"radiosOff":{"dataThresholdBytes":-1}}"#,
                "✖ Too small: expected number to be >=0\n  → at radiosOff.dataThresholdBytes",
            ],
            [
                r#"{"radiosOff":{"dataThresholdBytes":1.5}}"#,
                "✖ Invalid input: expected int, received number\n  → at radiosOff.dataThresholdBytes",
            ],
            [
                r#"{"radiosOff":{"dataThresholdBytes":"1"}}"#,
                "✖ Invalid input: expected number, received string\n  → at radiosOff.dataThresholdBytes",
            ],
            [
                r#"{"radiosOff":{"x":1,"y":2}}"#,
                "✖ Unrecognized keys: \"x\", \"y\"\n  → at radiosOff",
            ],
        ]);
    }

    #[test]
    fn value_errors_read_as_zod_wrote_them() {
        check_cases(&[
            [
                r#"{"removeApps":[1,"a.b","c"]}"#,
                "✖ Invalid input: expected string, received number\n  → at removeApps[0]\n\
                 ✖ an Android package name\n  → at removeApps[2]",
            ],
            [
                r#"{"screen":{"autoBrightness":"y","brightness":-1,"timeoutSeconds":0,"stayOnWhileCharging":null}}"#,
                "✖ Invalid input: expected boolean, received string\n  → at screen.autoBrightness\n\
                 ✖ Too small: expected number to be >=0\n  → at screen.brightness\n\
                 ✖ Too small: expected number to be >=1\n  → at screen.timeoutSeconds\n\
                 ✖ Invalid input: expected boolean, received null\n  → at screen.stayOnWhileCharging",
            ],
            [
                r#"{"screen":{"brightness":2.5,"timeoutSeconds":86401}}"#,
                "✖ Invalid input: expected int, received number\n  → at screen.brightness\n\
                 ✖ Too big: expected number to be <=86400\n  → at screen.timeoutSeconds",
            ],
            [
                r#"{"radioLogBufferMiB":65}"#,
                "✖ Too big: expected number to be <=64\n  → at radioLogBufferMiB",
            ],
            [
                r#"{"radioLogBufferMiB":true}"#,
                "✖ Invalid input: expected number, received boolean\n  → at radioLogBufferMiB",
            ],
            [
                r#"{"lockScreen":{"title":"  "}}"#,
                "✖ Too small: expected string to have >=1 characters\n  → at lockScreen.title\n\
                 ✖ one line\n  → at lockScreen.title",
            ],
            [
                r#"{"lockScreen":{"title":"two\nlines"}}"#,
                "✖ one line\n  → at lockScreen.title",
            ],
            [
                r#"{"lockScreen":{"title":"a\rb"}}"#,
                "✖ one line\n  → at lockScreen.title",
            ],
        ]);
    }

    #[test]
    fn string_errors_read_as_zod_wrote_them() {
        check_cases(&[
            [
                r#"{"lockScreen":{"title":"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"}}"#,
                "✖ Too big: expected string to have <=40 characters\n  → at lockScreen.title",
            ],
            [
                r#"{"lockScreen":{"lines":["a","b","c","d"],"title":""}}"#,
                "✖ Too small: expected string to have >=1 characters\n  → at lockScreen.title\n\
                 ✖ one line\n  → at lockScreen.title\n\
                 ✖ Too big: expected array to have <=3 items\n  → at lockScreen.lines",
            ],
            [
                r#"{"lockScreen":{"lines":["", "x\ny", 3]}}"#,
                "✖ Too small: expected string to have >=1 characters\n  → at lockScreen.lines[0]\n\
                 ✖ one line\n  → at lockScreen.lines[0]\n\
                 ✖ one line\n  → at lockScreen.lines[1]\n\
                 ✖ Invalid input: expected string, received number\n  → at lockScreen.lines[2]",
            ],
            [
                r#"{"lockScreen":{"subtitle":"x"}}"#,
                "✖ Unrecognized key: \"subtitle\"\n  → at lockScreen",
            ],
        ]);
    }

    #[test]
    fn issues_sort_by_path_length_then_schema_order_as_zod_sorted_them() {
        check_cases(&[
            [
                r#"{"b":1,"a":2,"screen":{"z":1}}"#,
                "✖ Unrecognized keys: \"b\", \"a\"\n✖ Unrecognized key: \"z\"\n  → at screen",
            ],
            [
                r#"{"radiosOff":{"x":1,"dataThresholdBytes":-1},"dataSaver":1}"#,
                "✖ Unrecognized key: \"x\"\n  → at radiosOff\n\
                 ✖ Invalid input: expected boolean, received number\n  → at dataSaver\n\
                 ✖ Too small: expected number to be >=0\n  → at radiosOff.dataThresholdBytes",
            ],
            [
                r#"{"lockScreen":{"q":1,"title":5},"screen":{"brightness":-1.5}}"#,
                "✖ Unrecognized key: \"q\"\n  → at lockScreen\n\
                 ✖ Invalid input: expected int, received number\n  → at screen.brightness\n\
                 ✖ Invalid input: expected string, received number\n  → at lockScreen.title",
            ],
            [
                r#"{"radiosOff":{"dataThresholdBytes":-1e20}}"#,
                "✖ Too small: expected int to be >=-9007199254740991\n  → at radiosOff.dataThresholdBytes\n\
                 ✖ Too small: expected number to be >=0\n  → at radiosOff.dataThresholdBytes",
            ],
            [
                r#"{"radiosOff":{"dataThresholdBytes":9007199254740992}}"#,
                "✖ Too big: expected int to be <=9007199254740991\n  → at radiosOff.dataThresholdBytes",
            ],
            [
                r#"{"2":1,"1":1,"b":1,"a":1}"#,
                "✖ Unrecognized keys: \"1\", \"2\", \"b\", \"a\"",
            ],
            [
                r#"{"screen":{"z":1},"disableSystemUpdates":"x"}"#,
                "✖ Unrecognized key: \"z\"\n  → at screen\n\
                 ✖ Invalid input: expected boolean, received string\n  → at disableSystemUpdates",
            ],
            [
                r#"{"removeApps":[1,2],"smsApp":3,"radiosOff":4,"zz":5}"#,
                "✖ Unrecognized key: \"zz\"\n\
                 ✖ Invalid input: expected object, received number\n  → at radiosOff\n\
                 ✖ Invalid input: expected string, received number\n  → at smsApp\n\
                 ✖ Invalid input: expected string, received number\n  → at removeApps[0]\n\
                 ✖ Invalid input: expected string, received number\n  → at removeApps[1]",
            ],
            ["{\"\\\"q\\\"\\n\":1}", "✖ Unrecognized key: \"\"q\"\n\""],
            [
                r#"{"removeApps":[null,{},[]]}"#,
                "✖ Invalid input: expected string, received null\n  → at removeApps[0]\n\
                 ✖ Invalid input: expected string, received object\n  → at removeApps[1]\n\
                 ✖ Invalid input: expected string, received array\n  → at removeApps[2]",
            ],
            [
                r#"{"smsApp":"c.1a"}"#,
                "✖ an Android package name\n  → at smsApp",
            ],
            [
                r#"{"smsApp":"c.a "}"#,
                "✖ an Android package name\n  → at smsApp",
            ],
        ]);
    }

    #[test]
    fn values_zod_accepted_pass() {
        for json in [
            r#"{"smsApp":"com.ex_ample.A1"}"#,
            r#"{"smsApp":"Com._x"}"#,
            r#"{"smsApp":"c._"}"#,
            r#"{"removeApps":["a.b","a.b"]}"#,
            r#"{"radiosOff":{"dataThresholdBytes":9007199254740991}}"#,
            r#"{"radiosOff":{"dataThresholdBytes":-0}}"#,
            r#"{"lockScreen":{"title":"😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀"}}"#,
            "{\"lockScreen\":{\"title\":\" \\ufeffa\\u0085\"}}",
            r#"{"lockScreen":{"title":"a\n"}}"#,
        ] {
            assert!(Profile::from_json("p", json).is_ok(), "{json}");
        }
        assert_eq!(
            Profile::from_json("p", "{\"lockScreen\":{\"title\":\" \\ufeffa\\u0085\"}}")
                .unwrap()
                .lock_screen
                .unwrap()
                .title,
            "a\u{85}",
            "JavaScript's trim removes a byte order mark but not a next-line character"
        );
    }

    #[test]
    fn the_lock_screen_notice_refuses_long_or_multi_line_text() {
        for lock_screen in [
            r#"{"title":"two\nlines"}"#,
            r#"{"title":"  "}"#,
            r#"{"title":"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"}"#,
            r#"{"lines":["xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"]}"#,
            r#"{"lines":["a","b","c","d"]}"#,
            r#"{"subtitle":"unknown key"}"#,
        ] {
            let json = format!(r#"{{"lockScreen":{lock_screen}}}"#);
            assert!(Profile::from_json("p", &json).is_err(), "{json}");
        }
    }

    #[test]
    fn the_name_is_the_base_name_without_json_as_node_gives_it() {
        for (path, name) in [
            ("/app/profiles/no-data.json", "no-data"),
            ("no-data", "no-data"),
            ("a/b.json/", "b"),
            ("x.JSON", "x.JSON"),
            ("a.json.json", "a.json"),
            ("/a/.json", ".json"),
            (".json", ""),
            (".json/", ".json"),
            ("/", ""),
            ("", ""),
            ("a//b.json//", "b"),
        ] {
            assert_eq!(profile_name(path), name, "for {path:?}");
        }
    }
}
