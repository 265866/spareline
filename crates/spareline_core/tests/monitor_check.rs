//! The drift check, Set up phone and their alerts and events, ported from the pure parts of
//! spareline-old's `monitor.test.ts`.
//!
//! The phone is a stand-in: the B30's recorded policy read, with each command's effect applied
//! to the recording, as the old `MonitorPhone` did. The driver below composes the crate's
//! decisions in the order the old `Monitor.runCheck` and `runProvision` ran them, which is the
//! order the server follows.

use std::collections::{BTreeMap, HashSet};
use std::time::Duration;

use regex::{Captures, Regex};
use serde_json::Value;
use spareline_core::Timestamp;
use spareline_core::monitor::{
    self, Alert, AlertKind, AlertSet, ApplyPlan, Busy, CheckContext, CheckFinish, CheckReason,
    CheckReport, CommandResult, DataOffCause, DataOffFailure, LOCK_SCREEN_DEX_PHONE_PATH,
    LockScreenNotice, MonitorEvent, PhonePolicy, Policy, PolicyAlertSource, ProvisionReport,
    Revert, Severity, SimChange, SimDataUsage, Snapshot, UidBytes,
};

const RECORDED: &str = include_str!("fixtures/nuu-s6702x/policy-read.txt");
const B30_PROFILE: &str = include_str!("fixtures/nuu-b30-no-data.json");
const SERIAL: &str = "B30";
const LOG_BUFFER_COMMAND: &str = "logcat -b main,system,radio,crash -G 8M";
const PRESET: [&str; 4] = [
    "settings put global mobile_data2 0",
    "settings put global mobile_data3 0",
    "settings put global mobile_data4 0",
    "settings put global mobile_data5 0",
];

/// The recorded B30, with the effects of the commands it ran.
struct Phone {
    order: Vec<String>,
    sections: BTreeMap<String, String>,
    stuck: HashSet<String>,
    fail_apply: Option<String>,
    fail_push: Option<String>,
    files: HashSet<String>,
    applied: Vec<String>,
    reads: usize,
    pushes: Vec<String>,
}

impl Phone {
    fn recorded() -> Self {
        let parsed = spareline_core::split_sections(RECORDED);
        Self {
            order: parsed
                .sections()
                .iter()
                .map(|section| section.name.clone())
                .collect(),
            sections: parsed
                .sections()
                .iter()
                .map(|section| (section.name.clone(), section.text.clone()))
                .collect(),
            stuck: HashSet::new(),
            fail_apply: None,
            fail_push: None,
            files: HashSet::new(),
            applied: Vec::new(),
            reads: 0,
            pushes: Vec::new(),
        }
    }

    fn section(&self, name: &str) -> String {
        self.sections.get(name).cloned().unwrap_or_default()
    }

    fn set_section(&mut self, name: &str, body: &str) {
        self.sections.insert(name.to_owned(), body.to_owned());
    }

    fn set(&mut self, namespace: &str, key: &str, value: &str) {
        let name = format!("settings:{namespace}");
        let prefix = format!("{key}=");
        let mut lines: Vec<String> = self
            .section(&name)
            .split('\n')
            .filter(|line| !line.is_empty() && !line.starts_with(&prefix))
            .map(str::to_owned)
            .collect();
        lines.push(format!("{key}={value}"));
        self.set_section(&name, &lines.join("\n"));
    }

    fn get(&self, namespace: &str, key: &str) -> Option<String> {
        let prefix = format!("{key}=");
        self.section(&format!("settings:{namespace}"))
            .split('\n')
            .find_map(|line| line.strip_prefix(&prefix).map(str::to_owned))
    }

    fn write(&mut self, namespace: &str, key: &str, value: &str) {
        if !self.stuck.contains(&format!("{namespace}:{key}")) {
            self.set(namespace, key, value);
        }
    }

    fn set_lock_wallpaper(&mut self, name: Option<&str>) {
        let entry = name.map_or_else(String::new, |name| {
            format!(
                " User 0: id=10: mWhich=2: mSystemWasBoth=true\n  mName={name}\n  \
                 mWallpaperComponent=null\n"
            )
        });
        let pattern = Regex::new(r"(Lock wallpaper state:\n)(?: .*\n)*")
            .expect("the recorded fixture has this");
        let wallpaper = format!("{}\n", self.section("wallpaper"));
        let replaced = pattern.replace(&wallpaper, |captures: &Captures<'_>| {
            format!("{}{entry}", &captures[1])
        });
        self.set_section("wallpaper", replaced.trim_end());
    }

    fn read(&mut self) -> String {
        use std::fmt::Write as _;

        self.reads += 1;
        let mut output = String::new();
        for name in &self.order {
            write!(output, "\n@@sl:{name}\n{}\n", self.section(name))
                .expect("writing to a String never fails");
        }
        output
    }

    fn push(&mut self, path: &str) -> Result<(), String> {
        if let Some(error) = &self.fail_push {
            return Err(error.clone());
        }
        self.pushes.push(path.to_owned());
        self.files.insert(path.to_owned());
        Ok(())
    }

    fn run(&mut self, script: &str) -> Result<String, String> {
        use std::fmt::Write as _;

        if let Some(error) = &self.fail_apply {
            return Err(error.clone());
        }
        let pattern = Regex::new(r#"@@sl:run:[0-9]+\\n'; (.*?) 2>&1; echo "rc=\$\?""#)
            .expect("the recorded fixture has this");
        let commands: Vec<String> = pattern
            .captures_iter(script)
            .map(|captures| captures[1].to_owned())
            .collect();
        let mut out = String::new();
        for (index, command) in commands.iter().enumerate() {
            self.applied.push(command.clone());
            let rc = self.effect(command);
            write!(out, "\n@@sl:run:{index}\nrc={rc}\n").expect("writing to a String never fails");
        }
        Ok(out)
    }

    fn effect(&mut self, command: &str) -> i32 {
        let put = Regex::new(r"^settings put (global|secure|system) (\S+) (\S+)$")
            .expect("the recorded fixture has this");
        if let Some(put) = put.captures(command) {
            self.write(&put[1], &put[2], &put[3]);
            return 0;
        }
        match command {
            "svc data disable" => {
                self.write("global", "mobile_data", "0");
                if let Some(sub) =
                    spareline_core::parse_isub(&self.section("isub")).default_data_sub_id
                    && sub >= 0
                {
                    self.write("global", &format!("mobile_data{sub}"), "0");
                }
                return 0;
            }
            "svc wifi disable" => {
                self.write("global", "wifi_on", "0");
                return 0;
            }
            "cmd bluetooth_manager disable" => {
                self.write("global", "bluetooth_on", "0");
                return 0;
            }
            "cmd bluetooth_manager wait-for-state:STATE_OFF" => return 0,
            "cmd netpolicy set restrict-background true" => {
                self.set_section("datasaver", "Restrict background status: enabled");
                return 0;
            }
            LOG_BUFFER_COMMAND => {
                if !self.stuck.contains("logd") {
                    self.big_radio_buffer();
                }
                return 0;
            }
            _ => {}
        }
        if let Some(app) = command.strip_prefix("pm uninstall --user 0 ") {
            let remaining: Vec<String> = self
                .section("bloat")
                .split('\n')
                .filter(|line| *line != format!("package:{app}"))
                .map(str::to_owned)
                .collect();
            self.set_section("bloat", &remaining.join("\n"));
            return 0;
        }
        if command.starts_with("cmd role add-role-holder ") {
            return 0;
        }
        let notice = Regex::new(r"^CLASSPATH=(\S+) app_process / spareline\.LockScreen (\S+) \S+$")
            .expect("the recorded fixture has this");
        if let Some(notice) = notice.captures(command) {
            if !self.files.remove(&notice[1]) {
                return 134;
            }
            self.set_lock_wallpaper(Some(&notice[2]));
            return 0;
        }
        panic!("no effect recorded for {command:?}");
    }

    fn big_radio_buffer(&mut self) {
        self.set_section(
            "radiobuf",
            "radio: ring buffer is 8 MiB (252 KiB consumed, 1 MiB readable), max entry is 5120 \
             B, max payload is 4068 B",
        );
    }

    fn with_metered_traffic(&mut self, bytes: u64, uid: i64) {
        let now: i64 = self
            .section("clock")
            .parse()
            .expect("the recorded fixture has this");
        let rows = format!(
            "  ident=[{{type=0, ratType=13, subscriberId=310260..., metered=true, \
             defaultNetwork=true, oemManaged=OEM_NONE, subId=1}}] uid={uid} set=DEFAULT tag=0x0\n    \
             NetworkStatsHistory: bucketDuration=7200\n      st={} rb={bytes} rp=900 tb=0 tp=0 \
             op=0\nInterfaceMapUpdater:",
            now - 3600
        );
        let netstats = self
            .section("netstats")
            .replacen("InterfaceMapUpdater:", &rows, 1);
        self.set_section("netstats", &netstats);
    }

    /// A SIM in slot 1 as `sub_id`, in the recorded isub line format.
    fn insert_sim(&mut self, sub_id: i64) {
        let isub = self.section("isub");
        let line = isub
            .split('\n')
            .find(|line| line.contains("SubscriptionInfoInternal: id=1 "))
            .expect("the recorded fixture has this");
        let second = line
            .replacen("id=1 ", &format!("id={sub_id} "), 1)
            .replacen("simSlotIndex=0", "simSlotIndex=1", 1)
            .replacen(
                "enabledMobileDataPolicies=2",
                "enabledMobileDataPolicies=",
                1,
            );
        let isub = isub.replacen(
            "Active subscriptions:\n",
            &format!("Active subscriptions:\n{second}\n"),
            1,
        );
        self.set_section("isub", &isub);
    }
}

/// The old monitor's runtime for one online phone, driven by the crate's decisions.
struct Driver {
    policy: Policy,
    phone: Phone,
    now: Timestamp,
    screen_session: bool,
    slots: Option<BTreeMap<u32, i64>>,
    alarmed: Vec<i64>,
    check: Option<CheckReport>,
    provision: Option<ProvisionReport>,
    data_off_failure: Option<DataOffFailure>,
    alerts: AlertSet,
    events: Vec<MonitorEvent>,
}

impl Driver {
    fn new(policy: Policy) -> Self {
        Self {
            policy,
            phone: Phone::recorded(),
            now: Timestamp::parse_iso("2026-10-22T08:00:00.000Z")
                .expect("the recorded fixture has this"),
            screen_session: false,
            slots: None,
            alarmed: Vec::new(),
            check: None,
            provision: None,
            data_off_failure: None,
            alerts: AlertSet::new(),
            events: Vec::new(),
        }
    }

    fn b30() -> Self {
        Self::new(
            Policy::load("nuu-b30-no-data.json", B30_PROFILE)
                .expect("the recorded fixture has this"),
        )
    }

    fn context(&self) -> CheckContext {
        CheckContext {
            screen_session: self.screen_session,
        }
    }

    fn read(&mut self) -> Result<Snapshot, String> {
        let stdout = self.phone.read();
        self.policy
            .parse_read(&stdout, 0, "")
            .map_err(|error| error.to_string())
    }

    fn observe_slots(&mut self, snapshot: &Snapshot) -> Vec<SimChange> {
        let after = snapshot.slot_map();
        let changes = self
            .slots
            .as_ref()
            .map_or_else(Vec::new, |before| monitor::diff_slots(before, &after));
        self.slots = Some(after);
        changes
    }

    fn apply(&mut self, apply: &ApplyPlan) -> Result<Vec<CommandResult>, String> {
        let mut apply = apply.clone();
        let uploads = apply.uploads().to_vec();
        for (index, upload) in uploads.iter().enumerate() {
            for file in &upload.files {
                if let Err(error) = self.phone.push(file.path) {
                    apply.push_failed(index, file.path, &error);
                    break;
                }
            }
        }
        let stdout = match apply.script() {
            Some(script) => self.phone.run(&script)?,
            None => String::new(),
        };
        Ok(apply.results(&stdout))
    }

    fn connect(&mut self) -> CheckReport {
        self.slots = None;
        self.run_check(CheckReason::Connect, &[])
    }

    fn run_check(&mut self, reason: CheckReason, known: &[SimChange]) -> CheckReport {
        let at = self.now;
        let context = self.context();
        let known_inserted = if self.policy.radios_off.is_some() {
            monitor::inserted_sims(known)
        } else {
            Vec::new()
        };
        let mut cause = monitor::data_off_cause(&known_inserted, &[]);
        let result = (|| -> Result<CheckReport, String> {
            let snapshot = self.read()?;
            let mut changes = known.to_vec();
            changes.extend(self.observe_slots(&snapshot));
            let inserted = if self.policy.radios_off.is_some() {
                monitor::inserted_sims(&changes)
            } else {
                Vec::new()
            };
            let before = monitor::evaluate(&snapshot, context, &self.policy.rules);
            let usage = self.policy.usage(&snapshot, at);
            cause = monitor::data_off_cause(&inserted, &usage).or(cause);
            let mut data_off_subs: Vec<i64> = usage
                .iter()
                .filter(|sim| sim.over)
                .map(|sim| sim.sub_id)
                .collect();
            data_off_subs.extend(inserted.iter().filter_map(|change| change.to));
            let batch = self
                .policy
                .drift_batch(&snapshot, context, reason, &data_off_subs)
                .map_err(|error| error.to_string())?;
            let (after, after_snapshot) = match &batch.apply {
                Some(apply) => {
                    self.apply(apply)?;
                    let after_snapshot = self.read()?;
                    (
                        monitor::evaluate(&after_snapshot, context, &self.policy.rules),
                        after_snapshot,
                    )
                }
                None => (before.clone(), snapshot.clone()),
            };
            let finished = self.policy.finish_check(&CheckFinish {
                reason,
                at,
                read_ms: 0,
                steps: &batch.steps,
                before: &before,
                after: &after,
                after_snapshot: &after_snapshot,
                usage,
                inserted: &inserted,
                alarmed: &self.alarmed,
            });
            if finished.data_off {
                self.data_off_failure = None;
            } else if let Some(failure) = finished.data_off_failure {
                self.data_off_failure = Some(DataOffFailure {
                    at: self.now,
                    cause: failure.cause,
                    error: failure.error,
                });
            }
            self.events.extend(finished.events);
            self.alarmed = finished.alarmed;
            Ok(finished.report)
        })();
        let report = result.unwrap_or_else(|error| {
            if let Some(cause) = cause {
                self.data_off_failure = Some(DataOffFailure {
                    at: self.now,
                    cause,
                    error: error.clone(),
                });
            }
            CheckReport::failed(at, reason, &error, self.check.as_ref())
        });
        self.check = Some(report.clone());
        self.refresh_alerts();
        report
    }

    fn run_provision(&mut self) -> ProvisionReport {
        let at = self.now;
        let context = self.context();
        let result = (|| -> Result<ProvisionReport, String> {
            let snapshot = self.read()?;
            self.observe_slots(&snapshot);
            let batch = self
                .policy
                .provision_batch(&snapshot, context)
                .map_err(|error| error.to_string())?;
            let ran = match &batch.apply {
                Some(apply) => self.apply(apply)?,
                None => Vec::new(),
            };
            let after = self.read()?;
            let results = monitor::evaluate(&after, context, &self.policy.rules);
            let report = ProvisionReport::finished(at, 0, &batch.steps, &ran, results.clone());
            self.check = Some(CheckReport {
                at,
                reason: CheckReason::Manual,
                read_ms: 0,
                results,
                usage: self.policy.usage(&after, at),
                reverted: Vec::new(),
                error: None,
            });
            Ok(report)
        })();
        let report = result.unwrap_or_else(|error| ProvisionReport::failed(at, 0, &error));
        self.events.push(MonitorEvent::Provision {
            report: report.clone(),
        });
        self.provision = Some(report.clone());
        self.refresh_alerts();
        report
    }

    fn refresh_alerts(&mut self) {
        let source = PolicyAlertSource {
            serial: SERIAL,
            online: true,
            data_off_failure: self.data_off_failure.as_ref(),
            check: self.check.as_ref(),
        };
        let next = Alert::for_policy(&[source], Some(&self.policy));
        let changes = self.alerts.update(next);
        for alert in changes.raised {
            self.events.push(MonitorEvent::AlertRaised { alert });
        }
        for alert in changes.cleared {
            self.events.push(MonitorEvent::AlertCleared { alert });
        }
    }

    fn details(&self, kind: &str) -> Vec<String> {
        self.events
            .iter()
            .filter(|event| event.kind() == kind)
            .map(MonitorEvent::detail_json)
            .collect()
    }

    fn alert_keys(&self) -> Vec<String> {
        self.alerts
            .alerts()
            .iter()
            .map(|alert| alert.key.clone())
            .collect()
    }
}

fn strings(list: &[&str]) -> Vec<String> {
    list.iter().map(|&item| item.to_owned()).collect()
}

fn fixed(rule: &str, before: &str, after: &str) -> Revert {
    Revert {
        rule: rule.to_owned(),
        before: before.to_owned(),
        after: after.to_owned(),
        fixed: true,
    }
}

fn unfixed(rule: &str, before: &str, after: &str) -> Revert {
    Revert {
        fixed: false,
        ..fixed(rule, before, after)
    }
}

#[test]
fn a_phone_coming_online_is_checked_and_gets_its_log_buffer_and_the_next_sims_data_keys() {
    let mut driver = Driver::b30();
    let check = driver.connect();
    let mut expected = strings(&PRESET);
    expected.push(LOG_BUFFER_COMMAND.to_owned());
    assert_eq!(driver.phone.applied, expected);
    assert_eq!(driver.phone.reads, 2);
    assert_eq!(check.reason, CheckReason::Connect);
    assert_eq!(check.error, None);
    assert_eq!(check.reverted, [fixed("log_buffer", "256 KiB", "8 MiB")]);
    assert!(check.results.iter().all(|result| result.ok));
    assert_eq!(
        driver.details("log_buffer_set"),
        [
            r#"{"rule":"log_buffer","before":"256 KiB","after":"8 MiB","fixed":true,"reason":"connect"}"#
        ]
    );
    assert_eq!(driver.details("drift"), Vec::<String>::new());
    assert_eq!(driver.details("alert.raised"), Vec::<String>::new());
    assert_eq!(driver.alerts.alerts(), []);
}

#[test]
fn a_phone_that_ignores_the_buffer_size_gets_a_quiet_card_item_retried_next_check() {
    let mut driver = Driver::b30();
    driver.phone.stuck.insert("logd".to_owned());
    driver.run_check(CheckReason::Manual, &[]);
    driver.run_check(CheckReason::Manual, &[]);
    assert_eq!(
        driver.phone.applied,
        [LOG_BUFFER_COMMAND, LOG_BUFFER_COMMAND]
    );
    assert_eq!(driver.details("log_buffer_set"), Vec::<String>::new());
    assert_eq!(driver.details("drift"), Vec::<String>::new());
    let alerts = driver.alerts.alerts();
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0].key, "policy:B30:log_buffer");
    assert_eq!(alerts[0].severity, Severity::Info);
    assert!(
        alerts[0]
            .message
            .ends_with("The phone ignored the buffer size; it will be retried on the next check.")
    );
    assert_eq!(
        alerts[0].message,
        "Radio log buffer at least 8 MiB: 256 KiB (want logcat -g -b radio reports a ring buffer \
         of 8 MiB or more). The phone ignored the buffer size; it will be retried on the next \
         check."
    );
    driver.phone.stuck.remove("logd");
    driver.run_check(CheckReason::Manual, &[]);
    assert_eq!(driver.phone.applied.len(), 3);
    assert_eq!(driver.alerts.alerts(), []);
    assert_eq!(
        driver.details("log_buffer_set"),
        [
            r#"{"rule":"log_buffer","before":"256 KiB","after":"8 MiB","fixed":true,"reason":"manual"}"#
        ]
    );
}

#[test]
fn first_sight_applies_only_what_the_drift_checker_fixes_safety_first() {
    let mut driver = Driver::b30();
    driver.phone.set("global", "wifi_scan_always_enabled", "1");
    driver.phone.set("system", "screen_off_timeout", "600000");
    driver
        .phone
        .set_section("bloat", "package:com.android.vending");
    driver.phone.set_section("smsrole", "com.android.messaging");
    let check = driver.connect();
    let mut expected = strings(&["settings put global wifi_scan_always_enabled 0"]);
    expected.extend(strings(&PRESET));
    expected.push(LOG_BUFFER_COMMAND.to_owned());
    assert_eq!(driver.phone.applied, expected);
    let failing: Vec<&str> = check
        .results
        .iter()
        .filter(|result| !result.ok)
        .map(|result| result.id.as_str())
        .collect();
    assert_eq!(failing, ["screen_timeout", "sms_role", "debloat"]);
}

#[test]
fn radio_drift_is_reverted_on_sight_and_logged_with_before_and_after() {
    let mut driver = Driver::b30();
    driver.connect();
    driver.phone.applied.clear();
    driver.events.clear();
    driver.phone.reads = 0;
    driver.phone.set("global", "wifi_scan_always_enabled", "1");
    driver.phone.set("secure", "location_mode", "3");
    let report = driver.run_check(CheckReason::Manual, &[]);
    assert_eq!(
        driver.phone.applied,
        [
            "settings put global wifi_scan_always_enabled 0",
            "settings put secure location_mode 0",
        ]
    );
    assert_eq!(
        driver
            .phone
            .get("global", "wifi_scan_always_enabled")
            .as_deref(),
        Some("0")
    );
    assert_eq!(
        report.reverted,
        [
            fixed("wifi_scan_off", "1", "0"),
            fixed("location_off", "3", "0"),
        ]
    );
    assert_eq!(
        driver.details("drift"),
        [
            r#"{"rule":"wifi_scan_off","before":"1","after":"0","fixed":true,"reason":"manual"}"#,
            r#"{"rule":"location_off","before":"3","after":"0","fixed":true,"reason":"manual"}"#,
        ]
    );
    assert_eq!(driver.phone.reads, 2, "read, apply, read again");
}

#[test]
fn other_drift_raises_an_alert_and_changes_nothing() {
    let mut driver = Driver::b30();
    driver.connect();
    driver.phone.applied.clear();
    driver.phone.set("system", "screen_brightness", "200");
    driver
        .phone
        .set("global", "ota_disable_automatic_update", "0");
    driver.run_check(CheckReason::Manual, &[]);
    assert_eq!(driver.phone.applied, Vec::<String>::new());
    assert_eq!(
        driver.details("alert.raised"),
        [
            r#"{"key":"policy:B30:ota_off","kind":"policy","severity":"warning","message":"Automatic system updates off: 0 (want global ota_disable_automatic_update = 1)."}"#,
            r#"{"key":"policy:B30:brightness_low","kind":"policy","severity":"info","message":"Brightness 10 of 255: 200 (want system screen_brightness = 10)."}"#,
        ]
    );
    driver.phone.set("system", "screen_brightness", "10");
    driver.run_check(CheckReason::Manual, &[]);
    assert_eq!(
        driver.details("alert.cleared"),
        [r#"{"key":"policy:B30:brightness_low","kind":"policy"}"#]
    );
}

#[test]
fn an_alert_keeps_its_first_since_while_its_key_lasts() {
    let mut driver = Driver::b30();
    driver.connect();
    driver
        .phone
        .set("global", "ota_disable_automatic_update", "0");
    driver.run_check(CheckReason::Manual, &[]);
    let since = driver.alerts.alerts()[0].since;
    driver.now = driver.now.plus(Duration::from_mins(5));
    driver.run_check(CheckReason::Interval, &[]);
    assert_eq!(driver.alerts.alerts()[0].since, since);
    assert_eq!(driver.check.as_ref().unwrap().at, driver.now);
}

#[test]
fn a_radio_that_will_not_turn_off_raises_a_critical_alert() {
    let mut driver = Driver::b30();
    driver.connect();
    driver.phone.set("global", "bluetooth_on", "1");
    driver.phone.stuck.insert("global:bluetooth_on".to_owned());
    let report = driver.run_check(CheckReason::Manual, &[]);
    assert_eq!(report.reverted, [unfixed("bluetooth_off", "1", "1")]);
    let alert = driver
        .alerts
        .alerts()
        .iter()
        .find(|alert| alert.kind == AlertKind::DriftUnfixed)
        .unwrap();
    assert_eq!(alert.key, "drift_unfixed:B30:bluetooth_off");
    assert_eq!(alert.severity, Severity::Critical);
    assert_eq!(
        alert.message,
        "Bluetooth off: still 1 after the automatic fix."
    );
}

#[test]
fn a_check_whose_commands_fail_records_the_error_and_raises_an_alert() {
    let mut driver = Driver::b30();
    driver.connect();
    driver.events.clear();
    driver.phone.set("global", "wifi_on", "1");
    driver.phone.fail_apply = Some("device offline".to_owned());
    let report = driver.run_check(CheckReason::Manual, &[]);
    assert_eq!(report.error.as_deref(), Some("device offline"));
    assert!(
        !report.results.is_empty(),
        "a failed check keeps the last results"
    );
    let alert = driver
        .alerts
        .alerts()
        .iter()
        .find(|alert| alert.kind == AlertKind::CheckFailed)
        .unwrap();
    assert_eq!(alert.key, "check_failed:B30");
    assert_eq!(alert.message, "Policy check failed: device offline");
    assert_eq!(driver.details("drift"), Vec::<String>::new());
    driver.phone.fail_apply = None;
    let next = driver.run_check(CheckReason::Manual, &[]);
    assert_eq!(next.error, None);
    assert!(
        driver
            .alerts
            .alerts()
            .iter()
            .all(|alert| alert.kind != AlertKind::CheckFailed)
    );
}

#[test]
fn a_read_with_no_settings_is_a_failed_check_not_drift() {
    let mut driver = Driver::b30();
    driver.connect();
    driver.phone.applied.clear();
    driver.events.clear();
    driver.phone.set_section("settings:global", "");
    let report = driver.run_check(CheckReason::Manual, &[]);
    assert_eq!(
        report.error.as_deref(),
        Some("policy read returned no global settings")
    );
    assert_eq!(driver.phone.applied, Vec::<String>::new());
    assert_eq!(driver.details("drift"), Vec::<String>::new());
    assert!(driver.alert_keys().contains(&"check_failed:B30".to_owned()));
}

#[test]
fn scrcpys_stay_awake_is_left_alone_while_a_screen_session_is_active() {
    let mut driver = Driver::b30();
    driver.connect();
    driver.screen_session = true;
    driver.phone.set("global", "stay_on_while_plugged_in", "3");
    driver.run_check(CheckReason::Manual, &[]);
    driver.run_provision();
    assert!(
        !driver
            .phone
            .applied
            .contains(&"settings put global stay_on_while_plugged_in 0".to_owned())
    );
    assert!(
        driver
            .alert_keys()
            .iter()
            .all(|key| !key.ends_with(":stay_on_off"))
    );
    driver.screen_session = false;
    driver.run_check(CheckReason::Manual, &[]);
    assert!(
        driver
            .alert_keys()
            .iter()
            .any(|key| key.ends_with(":stay_on_off"))
    );
}

#[test]
fn set_up_applies_every_enforceable_rule_once_and_a_second_run_changes_nothing() {
    let mut driver = Driver::b30();
    driver.phone.set("global", "wifi_on", "1");
    driver.phone.set("system", "screen_off_timeout", "600000");
    driver.phone.big_radio_buffer();
    driver
        .phone
        .set_section("datasaver", "Restrict background status: disabled");
    driver.phone.set_section(
        "bloat",
        "package:com.android.vending\npackage:com.google.android.youtube",
    );
    let first = driver.run_provision();
    assert!(first.ok);
    assert_eq!(first.error, None);
    let mut expected = strings(&["svc wifi disable"]);
    expected.extend(strings(&PRESET));
    expected.extend(strings(&[
        "settings put system screen_off_timeout 15000",
        "cmd netpolicy set restrict-background true",
        "pm uninstall --user 0 com.android.vending",
        "pm uninstall --user 0 com.google.android.youtube",
    ]));
    assert_eq!(driver.phone.applied, expected);
    let rules: Vec<&str> = first.steps.iter().map(|step| step.rule.as_str()).collect();
    assert_eq!(
        rules,
        ["wifi_off", "screen_timeout", "data_saver", "debloat"]
    );
    let second = driver.run_provision();
    assert!(second.ok);
    assert_eq!(second.steps, []);
    assert_eq!(driver.phone.applied.len(), expected.len());
    let changed: Vec<String> = driver.details("provision");
    assert_eq!(changed.len(), 2);
    assert!(
        changed[0].contains(r#""changed":["wifi_off","screen_timeout","data_saver","debloat"]"#)
    );
    assert!(changed[1].contains(r#""changed":[]"#));
    assert_eq!(driver.alerts.alerts(), []);
}

#[test]
fn a_changed_lock_screen_is_an_alert_until_set_up_pushes_the_program_and_draws_the_notice_again() {
    let mut driver = Driver::b30();
    driver.connect();
    driver.phone.applied.clear();
    driver
        .phone
        .set("secure", "lock_screen_show_notifications", "1");
    driver.phone.set_lock_wallpaper(None);
    driver.run_check(CheckReason::Manual, &[]);
    assert_eq!(driver.phone.applied, Vec::<String>::new());
    assert_eq!(driver.phone.pushes, Vec::<String>::new());
    let alerts: Vec<(String, Severity)> = driver
        .alerts
        .alerts()
        .iter()
        .map(|alert| (alert.key.clone(), alert.severity))
        .collect();
    assert_eq!(
        alerts,
        [
            (
                "policy:B30:lock_screen_notifications_off".to_owned(),
                Severity::Warning
            ),
            ("policy:B30:lock_screen_notice".to_owned(), Severity::Info),
        ]
    );
    let report = driver.run_provision();
    assert!(report.ok);
    let rules: Vec<&str> = report.steps.iter().map(|step| step.rule.as_str()).collect();
    assert_eq!(
        rules,
        ["lock_screen_notifications_off", "lock_screen_notice"]
    );
    assert_eq!(driver.phone.pushes, [LOCK_SCREEN_DEX_PHONE_PATH]);
    let notice = LockScreenNotice::default().command();
    assert_eq!(
        driver.phone.applied[driver.phone.applied.len() - 2..],
        [
            "settings put secure lock_screen_show_notifications 0".to_owned(),
            notice
        ]
    );
    assert_eq!(driver.alerts.alerts(), []);
    let applied = driver.phone.applied.len();
    driver.run_provision();
    assert_eq!(
        driver.phone.pushes.len(),
        1,
        "already drawn: nothing is pushed"
    );
    assert_eq!(
        driver.phone.applied.len(),
        applied,
        "already drawn: nothing runs"
    );
}

#[test]
fn a_failed_push_skips_only_the_notice_and_the_radio_fix_still_runs() {
    let mut driver = Driver::b30();
    driver.connect();
    driver.phone.applied.clear();
    driver.phone.set("global", "wifi_on", "1");
    driver.phone.set_lock_wallpaper(None);
    driver
        .phone
        .files
        .insert(LOCK_SCREEN_DEX_PHONE_PATH.to_owned());
    driver.phone.fail_push = Some("push: No space left on device".to_owned());
    let report = driver.run_provision();
    assert_eq!(driver.phone.applied[0], "svc wifi disable");
    assert!(
        driver
            .phone
            .applied
            .iter()
            .all(|command| !command.starts_with("CLASSPATH="))
    );
    assert_eq!(driver.phone.get("global", "wifi_on").as_deref(), Some("0"));
    assert!(!report.ok);
    let steps: Vec<(&str, Vec<(String, String)>)> = report
        .steps
        .iter()
        .map(|step| {
            (
                step.rule.as_str(),
                step.failed
                    .iter()
                    .map(|failed| (failed.command.clone(), failed.output.clone()))
                    .collect(),
            )
        })
        .collect();
    assert_eq!(
        steps,
        [
            ("wifi_off", Vec::new()),
            (
                "lock_screen_notice",
                vec![(
                    LockScreenNotice::default().command(),
                    "not run: pushing /data/local/tmp/spareline-lockscreen.dex failed: push: No \
                     space left on device"
                        .to_owned()
                )]
            ),
        ]
    );
    let failing: Vec<&str> = report
        .results
        .iter()
        .filter(|result| !result.ok)
        .map(|result| result.id.as_str())
        .collect();
    assert_eq!(failing, ["lock_screen_notice"]);
}

#[test]
fn internet_data_above_the_threshold_turns_data_off_again_and_turns_the_phone_red() {
    let mut driver = Driver::b30();
    driver.connect();
    driver.phone.applied.clear();
    driver.events.clear();
    driver.phone.with_metered_traffic(1_500_000, 10101);
    let report = driver.run_check(CheckReason::Manual, &[]);
    assert_eq!(
        report.usage,
        [SimDataUsage {
            sub_id: 1,
            slot: Some(0),
            rx: 1_500_000,
            tx: 0,
            over: true,
            uids: vec![UidBytes {
                uid: 10101,
                bytes: 1_500_000
            }],
        }]
    );
    assert_eq!(
        driver.phone.applied,
        [
            "svc data disable",
            "settings put global mobile_data 0",
            "settings put global mobile_data1 0",
        ]
    );
    assert_eq!(
        driver.details("data_threshold"),
        [
            r#"{"subId":1,"slot":0,"rx":1500000,"tx":0,"thresholdBytes":1000000,"uids":[{"uid":10101,"bytes":1500000}],"dataOff":true}"#
        ]
    );
    assert_eq!(driver.alarmed, [1]);
    let alert = driver
        .alerts
        .alerts()
        .iter()
        .find(|alert| alert.kind == AlertKind::DataThreshold)
        .unwrap();
    assert_eq!(alert.key, "data_threshold:B30:1");
    assert_eq!(
        alert.message,
        "SIM 1 used 1500000 bytes of mobile data in 24 h; data turned off again."
    );
    driver.run_check(CheckReason::Interval, &[]);
    assert_eq!(driver.phone.applied.len(), 6, "data is turned off again");
    assert_eq!(driver.details("data_threshold").len(), 1, "logged once");
}

#[test]
fn mms_and_video_call_traffic_of_the_phone_process_does_not_count() {
    let mut driver = Driver::new(
        Policy::load(
            "p.json",
            &B30_PROFILE.replace(
                "\"dataThresholdBytes\": 1000000",
                "\"dataThresholdBytes\": 1000",
            ),
        )
        .unwrap(),
    );
    assert_eq!(driver.policy.data_threshold_bytes(), Some(1000));
    driver.connect();
    driver.phone.applied.clear();
    driver.phone.with_metered_traffic(5_000_000, 1001);
    let report = driver.run_check(CheckReason::Manual, &[]);
    assert_eq!(report.usage, []);
    assert_eq!(driver.phone.applied, Vec::<String>::new());
    assert_eq!(driver.alarmed, Vec::<i64>::new());
}

#[test]
fn a_new_sim_gets_its_own_key_then_a_verified_check() {
    let mut driver = Driver::b30();
    driver.connect();
    driver.phone.applied.clear();
    driver.events.clear();
    driver.phone.insert_sim(9);
    let before = driver.slots.clone().unwrap();
    let isub = driver.phone.section("isub");
    let after = monitor::slot_map(&spareline_core::parse_isub(&isub).active);
    let changes = monitor::diff_slots(&before, &after);
    driver.slots = Some(after);
    let inserted = monitor::inserted_sims(&changes);
    let mut data_off: Vec<i64> = Vec::new();
    data_off.extend(inserted.iter().filter_map(|change| change.to));
    let commands = monitor::data_off_commands(&data_off);
    assert_eq!(
        commands,
        [
            "svc data disable",
            "settings put global mobile_data 0",
            "settings put global mobile_data9 0",
        ]
    );
    let plan = ApplyPlan::try_new(commands, Vec::new()).unwrap();
    driver.apply(&plan).unwrap();
    driver.run_check(CheckReason::Sim, &changes);
    assert_eq!(
        driver.details("sim.secured"),
        [r#"{"slot":1,"subId":9,"dataOff":true,"mmsWithDataOff":false}"#]
    );
    assert_eq!(
        driver.phone.get("global", "mobile_data9").as_deref(),
        Some("0")
    );
    let mms = driver
        .alerts
        .alerts()
        .iter()
        .find(|alert| alert.key.ends_with(":mms_with_data_off"))
        .unwrap();
    assert_eq!(mms.severity, Severity::Critical);
}

#[test]
fn a_failed_data_off_for_a_new_sim_raises_a_critical_alert_until_data_is_off() {
    let mut driver = Driver::b30();
    driver.connect();
    driver.phone.insert_sim(9);
    driver.phone.fail_apply = Some("adb: device offline".to_owned());
    let report = driver.run_check(CheckReason::Sim, &[]);
    assert_eq!(report.error.as_deref(), Some("adb: device offline"));
    let alert = driver
        .alerts
        .alerts()
        .iter()
        .find(|alert| alert.kind == AlertKind::SimUnsecured)
        .unwrap();
    assert_eq!(alert.key, "sim_unsecured:B30");
    assert_eq!(alert.severity, Severity::Critical);
    assert_eq!(
        alert.message,
        "A SIM changed and turning mobile data off failed: adb: device offline"
    );
    driver.phone.fail_apply = None;
    driver.run_check(CheckReason::Manual, &[]);
    assert!(
        driver
            .alerts
            .alerts()
            .iter()
            .all(|alert| alert.kind != AlertKind::SimUnsecured)
    );
    assert_eq!(
        driver.phone.get("global", "mobile_data9").as_deref(),
        Some("0")
    );
}

#[test]
fn a_failed_data_off_for_data_over_the_threshold_says_so() {
    let mut driver = Driver::b30();
    driver.connect();
    driver.phone.with_metered_traffic(1_500_000, 10101);
    driver.phone.fail_apply = Some("adb: device offline".to_owned());
    driver.run_check(CheckReason::Manual, &[]);
    assert_eq!(
        driver
            .data_off_failure
            .as_ref()
            .map(|failure| failure.cause),
        Some(DataOffCause::DataThreshold)
    );
    let alert = driver
        .alerts
        .alerts()
        .iter()
        .find(|alert| alert.kind == AlertKind::SimUnsecured)
        .unwrap();
    assert_eq!(
        alert.message,
        "Mobile data went over the threshold and turning mobile data off failed: adb: device \
         offline"
    );
}

#[test]
fn a_data_off_failure_alerts_while_the_phone_is_offline_and_other_policy_alerts_do_not() {
    let failure = DataOffFailure {
        at: Timestamp::from_millis(0),
        cause: DataOffCause::Sim,
        error: "adb: device offline".to_owned(),
    };
    let check = CheckReport::failed(Timestamp::from_millis(0), CheckReason::Sim, "x", None);
    let source = PolicyAlertSource {
        serial: SERIAL,
        online: false,
        data_off_failure: Some(&failure),
        check: Some(&check),
    };
    let alerts = Alert::for_policy(&[source], None);
    let kinds: Vec<AlertKind> = alerts.iter().map(|alert| alert.kind).collect();
    assert_eq!(kinds, [AlertKind::SimUnsecured]);
}

#[test]
fn the_report_and_alert_json_use_the_apis_names() {
    let mut driver = Driver::b30();
    driver.connect();
    driver
        .phone
        .set("global", "ota_disable_automatic_update", "0");
    let report = driver.run_check(CheckReason::Manual, &[]);
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json["at"], "2026-10-22T08:00:00.000Z");
    assert_eq!(json["reason"], "manual");
    assert_eq!(json["readMs"], 0);
    assert_eq!(json["error"], Value::Null);
    assert_eq!(
        json["results"][8],
        serde_json::json!({
            "id": "ota_off",
            "describe": "Automatic system updates off",
            "want": "global ota_disable_automatic_update = 1",
            "severity": "warning",
            "autoFix": false,
            "instruction": null,
            "fixable": true,
            "ok": false,
            "observed": "0",
        })
    );
    assert_eq!(
        serde_json::to_string(&driver.alerts.alerts()[0]).unwrap(),
        r#"{"key":"policy:B30:ota_off","kind":"policy","serial":"B30","severity":"warning","message":"Automatic system updates off: 0 (want global ota_disable_automatic_update = 1).","since":"2026-10-22T08:00:00.000Z"}"#
    );
    let phone = PhonePolicy {
        serial: SERIAL.to_owned(),
        busy: Some(Busy::Provision),
        check: None,
        provision: driver.provision.clone(),
        data_alarm: false,
    };
    assert_eq!(
        serde_json::to_string(&phone).unwrap(),
        r#"{"serial":"B30","busy":"provision","check":null,"provision":null,"dataAlarm":false}"#
    );
}

#[test]
fn the_last_provision_is_rebuilt_from_its_event() {
    let detail: Value = serde_json::from_str(
        r#"{"ok":true,"changed":["wifi_off","debloat"],"failedCommands":[],"failedRules":[],"durationMs":3100,"error":null}"#,
    )
    .unwrap();
    let report = ProvisionReport::from_event(Timestamp::from_millis(0), &detail);
    assert_eq!(
        serde_json::to_string(&report).unwrap(),
        r#"{"at":"1970-01-01T00:00:00.000Z","durationMs":3100,"steps":[{"rule":"wifi_off","commands":[],"failed":[]},{"rule":"debloat","commands":[],"failed":[]}],"results":[],"ok":true,"error":null}"#
    );
    let empty = ProvisionReport::from_event(Timestamp::from_millis(0), &serde_json::json!({}));
    assert_eq!(empty.duration_ms, 0);
    assert!(!empty.ok);
    assert_eq!(empty.steps, []);
}

#[test]
fn a_refused_command_fails_the_whole_batch() {
    let error = ApplyPlan::try_new(strings(&["svc data enable"]), Vec::new()).unwrap_err();
    assert_eq!(
        error.to_string(),
        "refusing to run \"svc data enable\" (svc ... enable)"
    );
}
