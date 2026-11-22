//! The phone policy a profile builds: its rules and the one batched read they need.

use std::collections::HashMap;
use std::time::Duration;

use serde::Serialize;

use crate::monitor::lock_wallpaper::LOCK_WALLPAPER_READ;
use crate::monitor::namespace::Namespace;
use crate::monitor::profile::{self, Profile, ProfileError, RadiosOff};
use crate::monitor::rule::{Rule, SMS_ROLE};
use crate::monitor::snapshot::Snapshot;
use crate::{android, text};

/// How long the batched policy read may take.
pub const POLICY_READ_TIMEOUT: Duration = Duration::from_mins(1);

/// How long an enforce script may take.
pub const ENFORCE_TIMEOUT: Duration = Duration::from_mins(2);

/// How many subscription ids past the highest one isub knows get `mobile_data<subId> 0` written
/// ahead of time.
pub const PRESET_SUB_IDS: i64 = 4;

/// The phone policy: the rules a profile turns on, in check order, and the read they share. The
/// drift checker and Set up phone run the same rules, so they cannot disagree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    /// The profile's file name without `.json`.
    pub name: String,
    /// The rules, in check order.
    pub rules: Vec<Rule>,
    /// Every read the rules and the data watch need, as one shell command line.
    pub read_script: String,
    /// The section the read prints last; its absence means a cut-short read.
    pub last_section: String,
    /// The settings tables the read lists; an empty one means the settings tool failed.
    pub namespaces: Vec<Namespace>,
    /// Radios and data off: data-off on SIM changes, the next SIMs' data keys, and the data
    /// watch with its threshold. `None` when the profile leaves radios alone.
    pub radios_off: Option<RadiosOff>,
    remove_apps: Vec<String>,
}

/// A policy read that can't be trusted.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[must_use]
pub enum PolicyReadError {
    /// The last section is missing.
    #[error("policy read was cut short (exit {exit_code}): {stderr}")]
    CutShort {
        /// The shell's exit code.
        exit_code: i64,
        /// Its standard error, trimmed.
        stderr: String,
    },
    /// `dumpsys isub` printed no subscription list.
    #[error(transparent)]
    Isub(#[from] IsubUnreadable),
    /// A settings table the rules read came back empty.
    #[error("policy read returned no {namespace} settings")]
    NoSettings {
        /// The table, such as `global`.
        namespace: &'static str,
    },
}

/// An isub dump without its subscription list: an error or a truncated dump, which would
/// otherwise read as every SIM removed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("dumpsys isub printed no subscription list")]
#[must_use]
pub struct IsubUnreadable;

/// Checks that an isub dump lists subscriptions, even none.
///
/// # Errors
///
/// [`IsubUnreadable`] when it has no `Active subscriptions:` line.
pub fn check_isub(isub: &str) -> Result<(), IsubUnreadable> {
    if isub.contains("Active subscriptions:") {
        Ok(())
    } else {
        Err(IsubUnreadable)
    }
}

/// The profile part of `GET /api/v1/status`: `{name, setUp, removesApps}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileSummary {
    /// The profile's name.
    pub name: String,
    /// What Set up phone sets: the description of every enforceable rule.
    pub set_up: Vec<String>,
    /// Set up phone removes apps, and their data.
    pub removes_apps: bool,
}

impl Policy {
    /// The policy a profile builds, named `name`.
    #[must_use]
    pub fn new(name: &str, profile: &Profile) -> Self {
        let rules = rules_for(profile);
        let namespaces: Vec<Namespace> = Namespace::ALL
            .into_iter()
            .filter(|namespace| {
                rules
                    .iter()
                    .flat_map(|rule| &rule.settings)
                    .any(|setting| setting.namespace == *namespace)
            })
            .collect();
        let reads = reads_for(profile, &rules, &namespaces);
        let last_section = reads
            .last()
            .map_or_else(|| "registry".to_owned(), |read| read.section.clone());
        let read_script: Vec<String> = reads
            .iter()
            .map(|read| {
                format!(
                    "{}; {}",
                    android::section_marker(&read.section),
                    read.command
                )
            })
            .collect();
        Self {
            name: name.to_owned(),
            rules,
            read_script: read_script.join("; "),
            last_section,
            namespaces,
            radios_off: profile.radios_off,
            remove_apps: profile.remove_apps.clone(),
        }
    }

    /// Reads and checks a profile's JSON text, read from `path`, and builds its policy, named
    /// after the file.
    ///
    /// # Errors
    ///
    /// [`ProfileError`] when the text isn't JSON or doesn't match the schema, with every
    /// problem named.
    pub fn load(path: &str, text: &str) -> Result<Self, ProfileError> {
        let profile = Profile::from_json(path, text)?;
        Ok(Self::new(&profile::profile_name(path), &profile))
    }

    /// Parses a read's standard output, without the checks [`Self::parse_read`] makes.
    #[must_use]
    pub fn parse(&self, stdout: &str) -> Snapshot {
        Snapshot::parse(stdout, &self.remove_apps)
    }

    /// Parses the output of [`Self::read_script`], refusing a read that can't be trusted.
    ///
    /// # Errors
    ///
    /// [`PolicyReadError::CutShort`] when the last section is missing, [`PolicyReadError::Isub`]
    /// when isub printed no subscription list, and [`PolicyReadError::NoSettings`] when a
    /// settings table the rules read came back empty, which means the settings tool failed, not
    /// that the settings are gone.
    pub fn parse_read(
        &self,
        stdout: &str,
        exit_code: i64,
        stderr: &str,
    ) -> Result<Snapshot, PolicyReadError> {
        let sections = android::split_sections(stdout);
        if sections.get(&self.last_section).is_none() {
            return Err(PolicyReadError::CutShort {
                exit_code,
                stderr: text::trim(stderr).to_owned(),
            });
        }
        check_isub(sections.get("isub").unwrap_or_default())?;
        let snapshot = self.parse(stdout);
        for &namespace in &self.namespaces {
            if snapshot
                .settings
                .get(&namespace)
                .is_none_or(HashMap::is_empty)
            {
                return Err(PolicyReadError::NoSettings {
                    namespace: namespace.as_str(),
                });
            }
        }
        Ok(snapshot)
    }

    /// The rule with this id.
    #[must_use]
    pub fn rule(&self, id: &str) -> Option<&Rule> {
        self.rules.iter().find(|rule| rule.id == id)
    }

    /// The data threshold in bytes per SIM per 24 h, or `None` without radios off.
    #[must_use]
    pub fn data_threshold_bytes(&self) -> Option<u64> {
        self.radios_off.map(|radios| radios.data_threshold_bytes)
    }

    /// The profile part of `GET /api/v1/status`.
    #[must_use]
    pub fn summary(&self) -> ProfileSummary {
        ProfileSummary {
            name: self.name.clone(),
            set_up: self
                .rules
                .iter()
                .filter(|rule| rule.fixable())
                .map(|rule| rule.describe.clone())
                .collect(),
            removes_apps: self.rules.iter().any(|rule| rule.id == "debloat"),
        }
    }
}

/// The rules a profile turns on, in check order.
fn rules_for(profile: &Profile) -> Vec<Rule> {
    let mut rules = Vec::new();
    if profile.radios_off.is_some() {
        rules.extend(Rule::radio_rules());
    }
    if profile.disable_system_updates {
        rules.push(Rule::ota_off());
    }
    rules.extend(Rule::screen_rules(&profile.screen));
    if profile.data_saver {
        rules.push(Rule::data_saver());
    }
    if let Some(app) = &profile.sms_app {
        rules.push(Rule::sms_role(app));
    }
    if !profile.remove_apps.is_empty() {
        rules.push(Rule::debloat(profile.remove_apps.len()));
    }
    if profile.mms_with_data_off {
        rules.push(Rule::mms());
    }
    if let Some(mib) = profile.radio_log_buffer_mib {
        rules.push(Rule::log_buffer(mib));
    }
    if let Some(notice) = &profile.lock_screen {
        rules.extend(Rule::lock_screen_rules(notice));
    }
    rules
}

/// The batched read's sections, in order: the settings tables the rules read, isub and the
/// registry, then what each group needs.
fn reads_for(profile: &Profile, rules: &[Rule], namespaces: &[Namespace]) -> Vec<Read> {
    let mut reads: Vec<Read> = Vec::new();
    for &namespace in namespaces {
        let keys: Vec<&str> = rules
            .iter()
            .flat_map(|rule| &rule.settings)
            .filter(|setting| setting.namespace == namespace)
            .map(|setting| setting.key.as_str())
            .collect();
        reads.push(Read::new(
            format!("settings:{}", namespace.as_str()),
            format!(
                "settings list {} | grep -E '^({})='",
                namespace.as_str(),
                keys.join("|")
            ),
        ));
    }
    reads.push(Read::new("isub".to_owned(), "dumpsys isub".to_owned()));
    reads.push(Read::new(
        "registry".to_owned(),
        "dumpsys telephony.registry | grep -E '^  Phone Id=|^    mIsDataEnabled='".to_owned(),
    ));
    if profile.data_saver {
        reads.push(Read::new(
            "datasaver".to_owned(),
            "cmd netpolicy get restrict-background".to_owned(),
        ));
    }
    if profile.sms_app.is_some() {
        reads.push(Read::new(
            "smsrole".to_owned(),
            format!("cmd role get-role-holders {SMS_ROLE}"),
        ));
    }
    if !profile.remove_apps.is_empty() {
        let patterns: Vec<String> = profile
            .remove_apps
            .iter()
            .map(|app| format!("-e package:{app}"))
            .collect();
        reads.push(Read::new(
            "bloat".to_owned(),
            format!(
                "pm list packages --user 0 | grep -xF {}",
                patterns.join(" ")
            ),
        ));
    }
    if profile.radio_log_buffer_mib.is_some() {
        reads.push(Read::new(
            "radiobuf".to_owned(),
            "logcat -g -b radio".to_owned(),
        ));
    }
    if profile.lock_screen.is_some() {
        reads.push(Read::new(
            "wallpaper".to_owned(),
            LOCK_WALLPAPER_READ.to_owned(),
        ));
    }
    if profile.radios_off.is_some() {
        reads.push(Read::new("clock".to_owned(), "date +%s".to_owned()));
        reads.push(Read::new(
            "netstats".to_owned(),
            "dumpsys netstats --full --uid".to_owned(),
        ));
    }
    reads
}

/// One section of the batched read.
#[derive(Debug)]
struct Read {
    section: String,
    command: String,
}

impl Read {
    const fn new(section: String, command: String) -> Self {
        Self { section, command }
    }
}

/// `mobile_data<id> 0` for the [`PRESET_SUB_IDS`] subscription ids after the highest one isub
/// knows, skipping keys already 0.
///
/// A guess the old code made: Android hands out subscription ids in sequence, so the next SIMs
/// this phone sees get the next ids. Writing their per-SIM data key as 0 ahead of time means a new
/// SIM starts with data off even before the SIM watch reacts. A key for an id that is never used
/// is harmless.
#[must_use]
pub fn preset_data_off_commands(snapshot: &Snapshot) -> Vec<String> {
    let highest = snapshot.known_sub_ids.iter().copied().fold(0, i64::max);
    (highest + 1..=highest + PRESET_SUB_IDS)
        .filter(|sub_id| {
            snapshot.setting(Namespace::Global, &format!("mobile_data{sub_id}")) != Some("0")
        })
        .map(|sub_id| format!("settings put global mobile_data{sub_id} 0"))
        .collect()
}

/// Turns mobile data off for the phone and for each listed sub, each sub once.
#[must_use]
pub fn data_off_commands(sub_ids: &[i64]) -> Vec<String> {
    let mut commands = vec![
        "svc data disable".to_owned(),
        "settings put global mobile_data 0".to_owned(),
    ];
    let mut seen: Vec<i64> = Vec::new();
    for &sub_id in sub_ids {
        if !seen.contains(&sub_id) {
            seen.push(sub_id);
            commands.push(format!("settings put global mobile_data{sub_id} 0"));
        }
    }
    commands
}
