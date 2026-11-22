//! The B30 profile's rules against the B30's recorded policy read, ported from spareline-old's
//! `policy.test.ts` and `profile.test.ts`.
//!
//! `policy-read.txt` is the B30 profile's read, recorded from a B30 that was already set up.
//! Variants change one recorded line, as a person tapping in Settings would; the phone itself
//! must not be put in those states on demand, since most of them turn a radio on.

use regex::{Captures, Regex};
use spareline_core::monitor::{
    self, CheckContext, CheckReason, LOCK_SCREEN_DEX_PHONE_PATH, LOCK_SCREEN_DEX_SHA256, Namespace,
    PhoneFile, PlanStep, Policy, Profile, RadiosOff, RuleResult, RuleSelection, Severity, Snapshot,
};

const RECORDED: &str = include_str!("fixtures/nuu-s6702x/policy-read.txt");
const B30_PROFILE: &str = include_str!("fixtures/nuu-b30-no-data.json");
const NO_DATA_PROFILE: &str = include_str!("fixtures/no-data.json");
const LOG_BUFFER_COMMAND: &str = "logcat -b main,system,radio,crash -G 8M";

const IDLE: CheckContext = CheckContext {
    screen_session: false,
};
const STREAMING: CheckContext = CheckContext {
    screen_session: true,
};

fn b30() -> Policy {
    Policy::load("/app/profiles/nuu-b30-no-data.json", B30_PROFILE)
        .expect("the recorded fixture has this")
}

fn recorded() -> Snapshot {
    b30().parse(RECORDED)
}

/// The recorded read with each `[from, to]` replaced once; a missing `from` fails the test.
fn variant(pairs: &[[&str; 2]]) -> Snapshot {
    let mut text = RECORDED.to_owned();
    for [from, to] in pairs {
        assert!(text.contains(from), "fixture has no {from}");
        text = text.replacen(from, to, 1);
    }
    b30().parse(&text)
}

/// The recorded read with the first match of each regex replaced, `$1` style.
fn variant_regex(pairs: &[[&str; 2]]) -> Snapshot {
    let mut text = RECORDED.to_owned();
    for [from, to] in pairs {
        let pattern = Regex::new(from).expect("the recorded fixture has this");
        assert!(pattern.is_match(&text), "fixture has no {from}");
        text = pattern.replace(&text, *to).into_owned();
    }
    b30().parse(&text)
}

/// A second SIM in slot 1 as sub 2, in the recorded isub line format.
fn with_second_sim(policies: &str) -> Snapshot {
    let pattern = Regex::new(r"(Active subscriptions:\n)(.*SubscriptionInfoInternal: id=1 .*)\n")
        .expect("the recorded fixture has this");
    let text = pattern.replace(RECORDED, |captures: &Captures<'_>| {
        let line = &captures[2];
        let second = line
            .replacen("id=1 ", "id=2 ", 1)
            .replacen("simSlotIndex=0", "simSlotIndex=1", 1)
            .replacen(
                "enabledMobileDataPolicies=2",
                &format!("enabledMobileDataPolicies={policies}"),
                1,
            );
        format!("{}{line}\n{second}\n", &captures[1])
    });
    b30().parse(&text)
}

fn results(snapshot: &Snapshot, context: CheckContext) -> Vec<RuleResult> {
    monitor::evaluate(snapshot, context, &b30().rules)
}

/// The verdict of one rule as `(ok, observed)`.
fn verdict(snapshot: &Snapshot, context: CheckContext, id: &str) -> Verdict {
    let result = results(snapshot, context)
        .into_iter()
        .find(|result| result.id == id)
        .expect("the recorded fixture has this");
    Verdict {
        ok: result.ok,
        observed: result.observed,
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Verdict {
    ok: bool,
    observed: String,
}

fn ok(observed: &str) -> Verdict {
    Verdict {
        ok: true,
        observed: observed.to_owned(),
    }
}

fn failing(observed: &str) -> Verdict {
    Verdict {
        ok: false,
        observed: observed.to_owned(),
    }
}

fn plan(snapshot: &Snapshot, context: CheckContext, selection: RuleSelection) -> Vec<PlanStep> {
    monitor::plan(snapshot, context, selection, &b30().rules)
}

fn step(rule: &'static str, commands: &[&str]) -> PlanStep {
    PlanStep {
        rule,
        commands: commands.iter().map(|&command| command.to_owned()).collect(),
    }
}

fn sections(read_script: &str) -> Vec<String> {
    Regex::new(r"@@[A-Za-z0-9_]+:([A-Za-z0-9_:]+)\\n")
        .expect("the recorded fixture has this")
        .captures_iter(read_script)
        .map(|captures| captures[1].to_owned())
        .collect()
}

#[test]
fn the_read_is_one_shell_call_covering_every_rule_and_the_data_watch() {
    let policy = b30();
    assert!(!policy.read_script.contains('\n'));
    assert_eq!(
        sections(&policy.read_script),
        [
            "settings:global",
            "settings:secure",
            "settings:system",
            "isub",
            "registry",
            "datasaver",
            "smsrole",
            "bloat",
            "radiobuf",
            "wallpaper",
            "clock",
            "netstats",
        ]
    );
    assert_eq!(policy.last_section, "netstats");
    let writes = Regex::new(r"settings put|svc |uninstall|netpolicy (set|add)|role add").unwrap();
    assert!(
        !writes.is_match(&policy.read_script),
        "the read is read-only"
    );
}

#[test]
fn the_read_script_is_the_exact_command_line_the_old_monitor_ran() {
    let policy = b30();
    let apps: Vec<String> = Profile::from_json("p", B30_PROFILE)
        .unwrap()
        .remove_apps
        .iter()
        .map(|app| format!("-e package:{app}"))
        .collect();
    let expected = [
        "printf '\\n@@sl:settings:global\\n'; settings list global | grep -E \
         '^(mobile_data[0-9]*|data_roaming[0-9]*|wifi_on|wifi_wakeup_enabled|wifi_scan_always_enabled|ble_scan_always_enabled|bluetooth_on|ota_disable_automatic_update|stay_on_while_plugged_in)='"
            .to_owned(),
        "printf '\\n@@sl:settings:secure\\n'; settings list secure | grep -E \
         '^(location_mode|lock_screen_show_notifications)='"
            .to_owned(),
        "printf '\\n@@sl:settings:system\\n'; settings list system | grep -E \
         '^(screen_brightness_mode|screen_brightness|screen_off_timeout)='"
            .to_owned(),
        "printf '\\n@@sl:isub\\n'; dumpsys isub".to_owned(),
        "printf '\\n@@sl:registry\\n'; dumpsys telephony.registry | grep -E '^  Phone Id=|^    \
         mIsDataEnabled='"
            .to_owned(),
        "printf '\\n@@sl:datasaver\\n'; cmd netpolicy get restrict-background".to_owned(),
        "printf '\\n@@sl:smsrole\\n'; cmd role get-role-holders android.app.role.SMS".to_owned(),
        format!(
            "printf '\\n@@sl:bloat\\n'; pm list packages --user 0 | grep -xF {}",
            apps.join(" ")
        ),
        "printf '\\n@@sl:radiobuf\\n'; logcat -g -b radio".to_owned(),
        "printf '\\n@@sl:wallpaper\\n'; dumpsys wallpaper | grep -E 'wallpaper state:|^ User \
         |mName=|mWallpaperComponent='"
            .to_owned(),
        "printf '\\n@@sl:clock\\n'; date +%s".to_owned(),
        "printf '\\n@@sl:netstats\\n'; dumpsys netstats --full --uid".to_owned(),
    ];
    assert_eq!(policy.read_script, expected.join("; "));
}

#[test]
fn the_provisioned_b30_passes_every_rule_but_the_radio_log_buffer_size() {
    let snapshot = recorded();
    assert_eq!(snapshot.radio_buffer_bytes, Some(256 * 1024));
    let expected = [
        ["mobile_data", "mobile_data=0, mobile_data1=0"],
        ["data_roaming", "data_roaming=0, data_roaming1=0"],
        ["wifi_off", "0"],
        ["wifi_wakeup_off", "0"],
        ["wifi_scan_off", "0"],
        ["ble_scan_off", "0"],
        ["bluetooth_off", "0"],
        ["location_off", "0"],
        ["ota_off", "1"],
        ["stay_on_off", "0"],
        ["brightness_manual", "0"],
        ["brightness_low", "10"],
        ["screen_timeout", "15000"],
        ["data_saver", "enabled"],
        ["sms_role", "com.google.android.apps.messaging"],
        ["debloat", "none installed"],
        ["mms_with_data_off", "SIM 1 (sub 1) on"],
        ["log_buffer", "256 KiB"],
        ["lock_screen_notifications_off", "0"],
        ["lock_screen_notice", "spareline-7d53969d19f0d42e"],
    ];
    let actual = results(&snapshot, IDLE);
    assert_eq!(actual.len(), expected.len());
    for (result, [id, observed]) in actual.iter().zip(expected) {
        assert_eq!(result.id, id);
        assert_eq!(result.observed, observed, "{id}");
        assert_eq!(result.ok, id != "log_buffer", "{id}");
    }
    let fix = [step("log_buffer", &[LOG_BUFFER_COMMAND])];
    assert_eq!(plan(&snapshot, IDLE, RuleSelection::All), fix);
    assert_eq!(plan(&snapshot, IDLE, RuleSelection::AutoFix), fix);
    assert_eq!(monitor::log_buffer_command(8), LOG_BUFFER_COMMAND);
    let resized = variant(&[["ring buffer is 256 KiB", "ring buffer is 8 MiB"]]);
    assert_eq!(verdict(&resized, IDLE, "log_buffer"), ok("8 MiB"));
    assert_eq!(plan(&resized, IDLE, RuleSelection::All), []);
}

#[test]
fn radio_drift_plans_exactly_the_commands_that_turn_it_off() {
    let drifted = variant(&[
        ["wifi_on=0", "wifi_on=1"],
        ["bluetooth_on=0", "bluetooth_on=1"],
        ["wifi_scan_always_enabled=0", "wifi_scan_always_enabled=1"],
        ["location_mode=0", "location_mode=3"],
        ["mobile_data1=0", "mobile_data1=1"],
        ["data_roaming=0", "data_roaming=1"],
        ["ring buffer is 256 KiB", "ring buffer is 8 MiB"],
    ]);
    assert_eq!(
        plan(&drifted, IDLE, RuleSelection::AutoFix),
        [
            step(
                "mobile_data",
                &[
                    "svc data disable",
                    "settings put global mobile_data 0",
                    "settings put global mobile_data1 0",
                ]
            ),
            step(
                "data_roaming",
                &[
                    "settings put global data_roaming 0",
                    "settings put global data_roaming1 0",
                ]
            ),
            step("wifi_off", &["svc wifi disable"]),
            step(
                "wifi_scan_off",
                &["settings put global wifi_scan_always_enabled 0"]
            ),
            step(
                "bluetooth_off",
                &[
                    "cmd bluetooth_manager disable",
                    "cmd bluetooth_manager wait-for-state:STATE_OFF",
                ]
            ),
            step("location_off", &["settings put secure location_mode 0"]),
        ]
    );
}

#[test]
fn a_new_sim_whose_per_sim_data_key_is_unset_counts_as_data_on() {
    let snapshot = with_second_sim("");
    assert_eq!(
        verdict(&snapshot, IDLE, "mobile_data"),
        failing("mobile_data=0, mobile_data1=0, mobile_data2=unset")
    );
    let mobile = plan(&snapshot, IDLE, RuleSelection::AutoFix)
        .into_iter()
        .find(|step| step.rule == "mobile_data")
        .unwrap();
    assert_eq!(
        mobile.commands,
        [
            "svc data disable",
            "settings put global mobile_data 0",
            "settings put global mobile_data1 0",
            "settings put global mobile_data2 0",
        ]
    );
    assert_eq!(
        verdict(&snapshot, IDLE, "mms_with_data_off"),
        failing("SIM 1 (sub 1) on, SIM 2 (sub 2) off")
    );
    assert!(verdict(&with_second_sim("1,2"), IDLE, "mms_with_data_off").ok);
}

#[test]
fn telephony_reporting_data_enabled_fails_the_data_rule_even_with_settings_off() {
    let snapshot = variant(&[["mIsDataEnabled=false", "mIsDataEnabled=true"]]);
    assert_eq!(
        verdict(&snapshot, IDLE, "mobile_data"),
        failing("mobile_data=0, mobile_data1=0, slot 0 data enabled")
    );
}

#[test]
fn check_only_rules_carry_the_on_phone_instruction_and_no_commands() {
    let snapshot = variant(&[["enabledMobileDataPolicies=2", "enabledMobileDataPolicies="]]);
    let mms = results(&snapshot, IDLE)
        .into_iter()
        .find(|result| result.id == "mms_with_data_off")
        .unwrap();
    assert!(!mms.ok);
    assert!(!mms.fixable);
    assert_eq!(
        mms.instruction.as_deref(),
        Some(
            "On the phone: Settings > Network & internet > SIMs > (the SIM) > turn on MMS \
             messages."
        )
    );
    assert!(
        plan(&snapshot, IDLE, RuleSelection::All)
            .iter()
            .all(|step| step.rule != "mms_with_data_off")
    );
}

#[test]
fn the_log_buffer_is_set_automatically_with_no_action_asked_of_the_owner() {
    let buffer = results(&recorded(), IDLE)
        .into_iter()
        .find(|result| result.id == "log_buffer")
        .unwrap();
    assert!(!buffer.ok);
    assert!(buffer.fixable);
    assert!(buffer.auto_fix);
    assert_eq!(buffer.severity, Severity::Info);
    assert_eq!(
        buffer.instruction.as_deref(),
        Some("The phone ignored the buffer size; it will be retried on the next check.")
    );
    assert_eq!(
        b30().rule("log_buffer").unwrap().fix_event,
        Some("log_buffer_set")
    );
}

#[test]
fn the_radio_buffer_passes_from_the_profiles_8_mib_by_the_size_logcat_reports_in_effect() {
    let sized = |size: &str| {
        verdict(
            &variant(&[["ring buffer is 256 KiB", &format!("ring buffer is {size}")]]),
            IDLE,
            "log_buffer",
        )
    };
    assert_eq!(sized("16 MiB"), ok("16 MiB"));
    assert_eq!(sized("8 MiB"), ok("8 MiB"));
    assert_eq!(sized("8191 KiB"), failing("8191 KiB"));
    assert_eq!(sized("4 MiB"), failing("4 MiB"));
}

#[test]
fn an_unreadable_buffer_size_says_so_instead_of_blaming_the_phone() {
    let unreadable = variant_regex(&[[
        r"radio: ring buffer is .*",
        "logcat: Unable to open log device",
    ]]);
    let result = results(&unreadable, IDLE)
        .into_iter()
        .find(|result| result.id == "log_buffer")
        .unwrap();
    assert!(!result.ok);
    assert_eq!(result.observed, "unreadable");
    assert_eq!(
        result.instruction.as_deref(),
        Some("Couldn't read the buffer size; it will be retried on the next check.")
    );
}

#[test]
fn scrcpys_stay_awake_setting_is_accepted_only_during_a_screen_session() {
    let awake = variant(&[["stay_on_while_plugged_in=0", "stay_on_while_plugged_in=3"]]);
    assert_eq!(verdict(&awake, IDLE, "stay_on_off"), failing("3"));
    assert_eq!(
        verdict(&awake, STREAMING, "stay_on_off"),
        ok("3 (screen session)")
    );
    assert!(
        plan(&awake, STREAMING, RuleSelection::All)
            .iter()
            .all(|step| step.rule != "stay_on_off")
    );
    assert_eq!(
        plan(&awake, IDLE, RuleSelection::All)
            .into_iter()
            .find(|step| step.rule == "stay_on_off")
            .unwrap()
            .commands,
        ["settings put global stay_on_while_plugged_in 0"]
    );
}

#[test]
fn debloat_uninstalls_only_listed_apps_that_are_still_installed() {
    let snapshot = variant(&[[
        "@@sl:bloat\n",
        "@@sl:bloat\npackage:com.android.chrome\npackage:com.example.notlisted\npackage:com.google.android.dialer\n",
    ]]);
    assert_eq!(
        verdict(&snapshot, IDLE, "debloat"),
        failing("installed: com.android.chrome, com.google.android.dialer")
    );
    let commands = plan(&snapshot, IDLE, RuleSelection::All)
        .into_iter()
        .find(|step| step.rule == "debloat")
        .unwrap()
        .commands;
    assert_eq!(
        commands,
        [
            "pm uninstall --user 0 com.android.chrome",
            "pm uninstall --user 0 com.google.android.dialer",
        ]
    );
    let listed = Profile::from_json("p", B30_PROFILE).unwrap().remove_apps;
    for command in commands {
        let app = command.replace("pm uninstall --user 0 ", "");
        assert!(listed.contains(&app), "{app} isn't in the profile");
    }
}

#[test]
fn no_rules_enforce_command_ever_enables_a_radio_or_data() {
    let everything_wrong = variant(&[
        ["wifi_on=0", "wifi_on=1"],
        ["bluetooth_on=0", "bluetooth_on=1"],
        ["wifi_wakeup_enabled=0", "wifi_wakeup_enabled=1"],
        ["wifi_scan_always_enabled=0", "wifi_scan_always_enabled=1"],
        ["ble_scan_always_enabled=0", "ble_scan_always_enabled=1"],
        ["location_mode=0", "location_mode=3"],
        ["mobile_data=0", "mobile_data=1"],
        ["mobile_data1=0", "mobile_data1=1"],
        ["data_roaming=0", "data_roaming=1"],
        [
            "ota_disable_automatic_update=1",
            "ota_disable_automatic_update=0",
        ],
        ["stay_on_while_plugged_in=0", "stay_on_while_plugged_in=7"],
        ["screen_brightness=10", "screen_brightness=255"],
        ["status: enabled", "status: disabled"],
        ["@@sl:bloat\n", "@@sl:bloat\npackage:com.android.vending\n"],
    ]);
    let policy = b30();
    let snapshots = [
        recorded(),
        everything_wrong,
        with_second_sim(""),
        policy.parse(""),
    ];
    let mut commands = 0;
    for snapshot in &snapshots {
        for context in [IDLE, STREAMING] {
            for rule in &policy.rules {
                for command in rule.enforce(snapshot, context).unwrap_or_default() {
                    commands += 1;
                    assert_eq!(monitor::radio_hazard(&command), None, "{command}");
                }
            }
        }
    }
    assert!(commands > 40, "only {commands} commands were checked");
}

#[test]
fn the_next_four_subscription_ids_get_their_data_key_preset_once() {
    let snapshot = recorded();
    assert!(snapshot.known_sub_ids.contains(&1));
    assert_eq!(
        monitor::preset_data_off_commands(&snapshot),
        [
            "settings put global mobile_data2 0",
            "settings put global mobile_data3 0",
            "settings put global mobile_data4 0",
            "settings put global mobile_data5 0",
        ]
    );
    let later = variant_regex(&[
        [
            "mobile_data1=0",
            "mobile_data1=0\nmobile_data2=0\nmobile_data8=1",
        ],
        [
            r"(All subscriptions:\n)",
            "${1}  [SubscriptionInfoInternal: id=7 iccId=890155500[****] simSlotIndex=-1 ]\n",
        ],
    ]);
    assert!(later.known_sub_ids.contains(&7));
    let preset = monitor::preset_data_off_commands(&later);
    assert_eq!(
        preset,
        [
            "settings put global mobile_data8 0",
            "settings put global mobile_data9 0",
            "settings put global mobile_data10 0",
            "settings put global mobile_data11 0",
        ]
    );
    for command in preset {
        assert_eq!(monitor::radio_hazard(&command), None);
    }
}

#[test]
fn safety_rules_are_exactly_the_radio_and_data_rules() {
    let policy = b30();
    let safety: Vec<&str> = policy
        .rules
        .iter()
        .filter(|rule| rule.safety)
        .map(|rule| rule.id)
        .collect();
    assert_eq!(
        safety,
        [
            "mobile_data",
            "data_roaming",
            "wifi_off",
            "wifi_wakeup_off",
            "wifi_scan_off",
            "ble_scan_off",
            "bluetooth_off",
            "location_off",
        ]
    );
    let critical_auto_fix: Vec<&str> = policy
        .rules
        .iter()
        .filter(|rule| rule.auto_fix && rule.severity == Severity::Critical)
        .map(|rule| rule.id)
        .collect();
    assert_eq!(critical_auto_fix, safety);
    assert!(!policy.rule("log_buffer").unwrap().safety);
}

#[test]
fn lock_screen_notifications_and_a_missing_notice_are_fixed_only_by_set_up_with_the_program_pushed()
{
    let policy = b30();
    let notice = policy.rule("lock_screen_notice").unwrap();
    let command = notice.enforce(&recorded(), IDLE).unwrap().remove(0);
    assert!(command.starts_with(&format!(
        "CLASSPATH={LOCK_SCREEN_DEX_PHONE_PATH} app_process / spareline.LockScreen \
         spareline-7d53969d19f0d42e "
    )));
    assert_eq!(
        notice.files,
        [PhoneFile {
            path: LOCK_SCREEN_DEX_PHONE_PATH,
            sha256: LOCK_SCREEN_DEX_SHA256
        }]
    );
    let changed = variant_regex(&[
        [
            "lock_screen_show_notifications=0",
            "lock_screen_show_notifications=1",
        ],
        [r"mName=spareline-[A-Za-z0-9_]+", "mName="],
    ]);
    assert_eq!(
        verdict(&changed, IDLE, "lock_screen_notifications_off"),
        failing("1")
    );
    assert_eq!(
        verdict(&changed, IDLE, "lock_screen_notice"),
        failing("an unnamed wallpaper")
    );
    let lock_steps: Vec<PlanStep> = plan(&changed, IDLE, RuleSelection::All)
        .into_iter()
        .filter(|step| step.rule.starts_with("lock_screen"))
        .collect();
    assert_eq!(
        lock_steps,
        [
            step(
                "lock_screen_notifications_off",
                &["settings put secure lock_screen_show_notifications 0"]
            ),
            step("lock_screen_notice", &[&command]),
        ]
    );
    assert!(
        plan(&changed, IDLE, RuleSelection::AutoFix)
            .iter()
            .all(|step| step.rule != "lock_screen_notice")
    );
    assert_eq!(
        verdict(
            &variant_regex(&[[r"(Lock wallpaper state:\n)(?: .*\n)*", "$1"]]),
            IDLE,
            "lock_screen_notice"
        ),
        failing("the home screen wallpaper")
    );
    assert_eq!(
        verdict(
            &variant_regex(&[[
                r"(mName=spareline-[A-Za-z0-9_]+\n {2}mWallpaperComponent=)null",
                "${1}ComponentInfo{com.example.live/com.example.live.Engine}",
            ]]),
            IDLE,
            "lock_screen_notice"
        ),
        failing("a live wallpaper (ComponentInfo{com.example.live/com.example.live.Engine})")
    );
    assert_eq!(
        verdict(
            &variant_regex(&[[r"@@sl:wallpaper\n[^@]*", "@@sl:wallpaper\n"]]),
            IDLE,
            "lock_screen_notice"
        ),
        failing("unreadable")
    );
}

#[test]
fn the_b30_example_profile_turns_on_every_rule_group_in_check_order() {
    let policy = b30();
    assert_eq!(policy.name, "nuu-b30-no-data");
    let shape: Vec<(&str, Severity, bool, bool)> = policy
        .rules
        .iter()
        .map(|rule| (rule.id, rule.severity, rule.auto_fix, rule.safety))
        .collect();
    let critical = Severity::Critical;
    let warning = Severity::Warning;
    let info = Severity::Info;
    assert_eq!(
        shape,
        [
            ("mobile_data", critical, true, true),
            ("data_roaming", critical, true, true),
            ("wifi_off", critical, true, true),
            ("wifi_wakeup_off", critical, true, true),
            ("wifi_scan_off", critical, true, true),
            ("ble_scan_off", critical, true, true),
            ("bluetooth_off", critical, true, true),
            ("location_off", critical, true, true),
            ("ota_off", warning, false, false),
            ("stay_on_off", info, false, false),
            ("brightness_manual", info, false, false),
            ("brightness_low", info, false, false),
            ("screen_timeout", info, false, false),
            ("data_saver", warning, false, false),
            ("sms_role", critical, false, false),
            ("debloat", warning, false, false),
            ("mms_with_data_off", critical, false, false),
            ("log_buffer", info, true, false),
            ("lock_screen_notifications_off", warning, false, false),
            ("lock_screen_notice", info, false, false),
        ]
    );
    let wants: Vec<&str> = policy.rules[9..13]
        .iter()
        .map(|rule| rule.want.as_str())
        .collect();
    assert_eq!(
        wants,
        [
            "global stay_on_while_plugged_in = 0",
            "system screen_brightness_mode = 0",
            "system screen_brightness = 10",
            "system screen_off_timeout = 15000",
        ]
    );
    assert_eq!(
        policy.radios_off,
        Some(RadiosOff {
            data_threshold_bytes: 1_000_000
        })
    );
}

#[test]
fn every_rules_exact_describe_and_want() {
    let policy = b30();
    let texts: Vec<[&str; 3]> = policy
        .rules
        .iter()
        .map(|rule| [rule.id, rule.describe.as_str(), rule.want.as_str()])
        .collect();
    assert_eq!(
        texts,
        [
            [
                "mobile_data",
                "Mobile data off, on every SIM",
                "global mobile_data and mobile_data<subId> = 0"
            ],
            [
                "data_roaming",
                "Data roaming off, on every SIM",
                "global data_roaming and data_roaming<subId> = 0"
            ],
            ["wifi_off", "Wi-Fi off", "global wifi_on = 0"],
            [
                "wifi_wakeup_off",
                "Wi-Fi does not turn itself back on",
                "global wifi_wakeup_enabled = 0"
            ],
            [
                "wifi_scan_off",
                "Wi-Fi scanning off",
                "global wifi_scan_always_enabled = 0"
            ],
            [
                "ble_scan_off",
                "Bluetooth scanning off",
                "global ble_scan_always_enabled = 0"
            ],
            ["bluetooth_off", "Bluetooth off", "global bluetooth_on = 0"],
            ["location_off", "Location off", "secure location_mode = 0"],
            [
                "ota_off",
                "Automatic system updates off",
                "global ota_disable_automatic_update = 1"
            ],
            [
                "stay_on_off",
                "Screen does not stay on while charging",
                "global stay_on_while_plugged_in = 0"
            ],
            [
                "brightness_manual",
                "Manual brightness",
                "system screen_brightness_mode = 0"
            ],
            [
                "brightness_low",
                "Brightness 10 of 255",
                "system screen_brightness = 10"
            ],
            [
                "screen_timeout",
                "Screen turns off after 15 s",
                "system screen_off_timeout = 15000"
            ],
            ["data_saver", "Data Saver on", "restrict-background enabled"],
            [
                "sms_role",
                "Google Messages is the default SMS app",
                "com.google.android.apps.messaging"
            ],
            [
                "debloat",
                "Unneeded apps removed",
                "none of the 37 listed apps installed for user 0"
            ],
            [
                "mms_with_data_off",
                "MMS works with mobile data off, on every SIM",
                "enabledMobileDataPolicies includes 2 (MMS_ALWAYS_ALLOWED)"
            ],
            [
                "log_buffer",
                "Radio log buffer at least 8 MiB",
                "logcat -g -b radio reports a ring buffer of 8 MiB or more"
            ],
            [
                "lock_screen_notifications_off",
                "Notifications hidden on the lock screen",
                "secure lock_screen_show_notifications = 0"
            ],
            [
                "lock_screen_notice",
                "Lock screen shows \"Managed by spareline\"",
                "lock screen wallpaper named spareline-7d53969d19f0d42e"
            ],
        ]
    );
}

#[test]
fn a_profile_turns_on_only_the_groups_it_names_and_reads_only_what_they_need() {
    let empty = Policy::new("empty", &Profile::default());
    assert_eq!(empty.rules, []);
    assert_eq!(empty.radios_off, None);
    assert_eq!(empty.namespaces, []);
    assert_eq!(sections(&empty.read_script), ["isub", "registry"]);
    assert_eq!(empty.last_section, "registry");

    let screen = Policy::new(
        "screen",
        &Profile::from_json(
            "p",
            r#"{"screen":{"timeoutSeconds":30,"autoBrightness":true}}"#,
        )
        .unwrap(),
    );
    let rules: Vec<[&str; 2]> = screen
        .rules
        .iter()
        .map(|rule| [rule.id, rule.want.as_str()])
        .collect();
    assert_eq!(
        rules,
        [
            ["brightness_manual", "system screen_brightness_mode = 1"],
            ["screen_timeout", "system screen_off_timeout = 30000"],
        ]
    );
    assert!(screen.read_script.contains(
        "settings list system | grep -E '^(screen_brightness_mode|screen_off_timeout)='"
    ));
    assert_eq!(
        sections(&screen.read_script),
        ["settings:system", "isub", "registry"]
    );
    assert_eq!(screen.namespaces, [Namespace::System]);
    assert!(
        screen
            .read_script
            .contains(&spareline_core::section_marker("registry"))
    );
}

#[test]
fn every_shipped_profile_loads_and_no_data_keeps_radios_off_without_device_tuning() {
    let no_data = Policy::load("/app/profiles/no-data.json", NO_DATA_PROFILE).unwrap();
    assert_eq!(no_data.name, "no-data");
    assert_eq!(b30().name, "nuu-b30-no-data");
    assert_eq!(no_data.data_threshold_bytes(), Some(1_000_000));
    let ids: Vec<&str> = no_data.rules.iter().map(|rule| rule.id).collect();
    assert_eq!(
        ids,
        [
            "mobile_data",
            "data_roaming",
            "wifi_off",
            "wifi_wakeup_off",
            "wifi_scan_off",
            "ble_scan_off",
            "bluetooth_off",
            "location_off",
            "ota_off",
            "data_saver",
            "mms_with_data_off",
        ]
    );
}

#[test]
fn the_lock_screen_notice_has_default_text() {
    let notice = Policy::new(
        "notice",
        &Profile::from_json("p", r#"{"lockScreen":{}}"#).unwrap(),
    );
    let rules: Vec<[&str; 2]> = notice
        .rules
        .iter()
        .map(|rule| [rule.id, rule.describe.as_str()])
        .collect();
    assert_eq!(
        rules,
        [
            [
                "lock_screen_notifications_off",
                "Notifications hidden on the lock screen"
            ],
            [
                "lock_screen_notice",
                "Lock screen shows \"Managed by spareline\""
            ],
        ]
    );
    assert_eq!(
        sections(&notice.read_script),
        ["settings:secure", "isub", "registry", "wallpaper"]
    );
    assert!(
        notice
            .read_script
            .contains("settings list secure | grep -E '^(lock_screen_show_notifications)='")
    );
}

#[test]
fn a_bad_profile_is_refused_with_every_problem_named() {
    let error = Policy::load(
        "test.json",
        r#"{"radiosOf":{},"removeApps":["not a package; rm -rf /"],"screen":{"brightness":300}}"#,
    )
    .unwrap_err()
    .to_string();
    assert!(error.starts_with("invalid policy profile test.json:\n"));
    for name in ["radiosOf", "removeApps", "screen.brightness"] {
        assert!(error.contains(name), "{error}");
    }
    assert!(
        Policy::load("test.json", "{ not json")
            .unwrap_err()
            .to_string()
            .starts_with("cannot read policy profile test.json: ")
    );
}

#[test]
fn the_status_summary_lists_what_set_up_sets() {
    let summary = b30().summary();
    assert_eq!(summary.name, "nuu-b30-no-data");
    assert!(summary.removes_apps);
    assert_eq!(summary.set_up.len(), 19);
    assert!(
        !summary
            .set_up
            .iter()
            .any(|describe| describe.starts_with("MMS"))
    );
    assert_eq!(
        serde_json::to_value(
            Policy::load("no-data.json", NO_DATA_PROFILE)
                .unwrap()
                .summary()
        )
        .unwrap(),
        serde_json::json!({
            "name": "no-data",
            "setUp": [
                "Mobile data off, on every SIM",
                "Data roaming off, on every SIM",
                "Wi-Fi off",
                "Wi-Fi does not turn itself back on",
                "Wi-Fi scanning off",
                "Bluetooth scanning off",
                "Bluetooth off",
                "Location off",
                "Automatic system updates off",
                "Data Saver on",
            ],
            "removesApps": false,
        })
    );
}

#[test]
fn a_read_that_cannot_be_trusted_is_refused() {
    let policy = b30();
    assert!(policy.parse_read(RECORDED, 0, "").is_ok());
    let cut = RECORDED.split("@@sl:netstats").next().unwrap();
    assert_eq!(
        policy
            .parse_read(cut, 137, " killed \n")
            .unwrap_err()
            .to_string(),
        "policy read was cut short (exit 137): killed"
    );
    let no_isub = RECORDED.replacen("Active subscriptions:", "Active subs:", 1);
    assert_eq!(
        policy.parse_read(&no_isub, 0, "").unwrap_err().to_string(),
        "dumpsys isub printed no subscription list"
    );
    let no_global = Regex::new(r"(@@sl:settings:global\n)[^@]*")
        .unwrap()
        .replace(RECORDED, "$1\n");
    assert_eq!(
        policy
            .parse_read(&no_global, 0, "")
            .unwrap_err()
            .to_string(),
        "policy read returned no global settings"
    );
}

#[test]
fn a_garbled_slot_index_is_shown_rather_than_aborting_the_check() {
    let snapshot = variant(&[["simSlotIndex=0", "simSlotIndex=4294967295"]]);
    let results = results(&snapshot, IDLE);
    let mms = results
        .iter()
        .find(|result| result.id == "mms_with_data_off")
        .expect("the B30 profile has the MMS rule");
    assert!(
        !mms.observed.contains("SIM 0 "),
        "the slot reads as printed, not wrapped: {}",
        mms.observed
    );
}

#[test]
fn a_garbled_subscription_id_presets_nothing_rather_than_aborting() {
    let snapshot = variant_regex(&[[
        r"(All subscriptions:\n)",
        "${1}  [SubscriptionInfoInternal: id=99999999999999999999 iccId=890155500[****] simSlotIndex=-1 ]\n",
    ]]);
    assert_eq!(
        monitor::preset_data_off_commands(&snapshot),
        Vec::<String>::new()
    );
}

#[test]
fn without_radios_off_a_check_plans_no_data_off_whatever_it_is_given() {
    let policy = Policy::load("/app/profiles/screen-only.json", r#"{"dataSaver":true}"#)
        .expect("a profile without radiosOff loads");
    let batch = policy
        .drift_batch(&recorded(), IDLE, CheckReason::Sim, &[1])
        .expect("nothing is refused");
    let commands = batch
        .apply
        .map_or_else(Vec::new, |apply| apply.commands().to_vec());
    assert!(
        commands
            .iter()
            .all(|command| !command.contains("mobile_data")),
        "no data-off without radiosOff: {commands:?}"
    );
}
