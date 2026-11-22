//! The data watch and the SIM watch against output recorded from the NUU B30. Mobile data stays
//! off on the real phone, so internet traffic is a stand-in: rows in the recorded per-uid format.

use std::fs;
use std::path::Path;

use spareline_core::{
    DATA_WINDOW_SECONDS, NetEntry, NetSection, PHONE_UID, SimLogScanner, SimUsage, UidBytes,
};

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/nuu-s6702x")
        .join(name);
    fs::read_to_string(&path).expect("the fixture is readable")
}

/// The recorded netstats section and the phone's clock when it was read.
struct Recording {
    netstats: String,
    now: i64,
}

fn recording() -> Recording {
    let sections = spareline_core::split_sections(&fixture("policy-read.txt"));
    Recording {
        netstats: sections
            .get("netstats")
            .expect("a netstats section")
            .to_owned(),
        now: sections
            .get("clock")
            .and_then(|clock| clock.parse().ok())
            .expect("a clock section"),
    }
}

/// The netstats text with a metered mobile row for `uid` before `InterfaceMapUpdater:`: a
/// two-hour bucket that starts an hour before `now`, with `bytes` received.
fn with_metered_traffic(netstats: &str, bytes: u64, now: i64, uid: i64) -> String {
    let rows = format!(
        "  ident=[{{type=0, ratType=13, subscriberId=310260..., metered=true, defaultNetwork=true, \
         oemManaged=OEM_NONE, subId=1}}] uid={uid} set=DEFAULT tag=0x0\n    \
         NetworkStatsHistory: bucketDuration=7200\n      st={} rb={bytes} rp=900 tb=0 tp=0 op=0\n",
        now - 3600
    );
    let index = netstats
        .find("\nInterfaceMapUpdater:")
        .map_or(netstats.len(), |index| index + 1);
    format!("{}{rows}{}", &netstats[..index], &netstats[index..])
}

fn usage(netstats: &str, now: i64) -> Vec<SimUsage> {
    spareline_core::internet_usage(
        &spareline_core::parse_netstats(netstats),
        now,
        DATA_WINDOW_SECONDS,
    )
}

#[test]
fn the_b30s_netstats_only_ims_traffic_on_unmetered_mobile_networks() {
    let Recording { netstats, now } = recording();
    let entries = spareline_core::parse_netstats(&netstats);
    let xt: Vec<&NetEntry> = entries
        .iter()
        .filter(|entry| entry.section == NetSection::Xt)
        .collect();
    assert!(!xt.is_empty(), "the recording has per-network entries");
    for entry in xt {
        assert!(entry.mobile && !entry.metered, "IMS on unmetered mobile");
        assert_eq!(entry.sub_id, Some(1));
        assert_eq!(entry.uid, -1);
        assert_eq!(entry.bucket_seconds, 3600);
    }
    assert!(
        entries
            .iter()
            .any(|entry| entry.section == NetSection::Uid && entry.uid == 0),
        "the per-uid history lists uid 0"
    );
    assert_eq!(usage(&netstats, now), []);
}

#[test]
fn internet_bytes_over_24_hours_are_summed_per_sim_with_the_apps() {
    let Recording { netstats, now } = recording();
    let text = with_metered_traffic(&netstats, 600_000, now, 10_101);
    let text = with_metered_traffic(&text, 50_000, now, 10_190);
    assert_eq!(
        usage(&text, now),
        [SimUsage {
            sub_id: 1,
            rx: 650_000,
            tx: 0,
            uids: vec![
                UidBytes {
                    uid: 10_101,
                    bytes: 600_000,
                },
                UidBytes {
                    uid: 10_190,
                    bytes: 50_000,
                },
            ],
        }]
    );
}

#[test]
fn the_phone_process_and_tagged_rows_do_not_count() {
    let Recording { netstats, now } = recording();
    let text = with_metered_traffic(&netstats, 9_000_000, now, PHONE_UID);
    let tagged = format!(
        "  ident=[{{type=0, ratType=13, subscriberId=310260..., metered=true, defaultNetwork=true, \
         oemManaged=OEM_NONE, subId=1}}] uid=10101 set=DEFAULT tag=0xffffff01\n    \
         NetworkStatsHistory: bucketDuration=7200\n      st={} rb=900000 rp=400 tb=0 tp=0 op=0\n\
         InterfaceMapUpdater:",
        now - 3600
    );
    let text = text.replacen("InterfaceMapUpdater:", &tagged, 1);
    assert_eq!(usage(&text, now), []);
}

#[test]
fn buckets_that_ended_before_the_window_do_not_count() {
    let Recording { netstats, now } = recording();
    let entries =
        spareline_core::parse_netstats(&with_metered_traffic(&netstats, 700_000, now, 10_101));
    assert_eq!(
        spareline_core::internet_usage(&entries, now + 3600 + 10, 5),
        []
    );
    assert_eq!(
        spareline_core::internet_usage(&entries, now + 3600 - 10, 600)[0].rx,
        700_000
    );
}

#[test]
fn wifi_rows_count_nothing() {
    let Recording { netstats, now } = recording();
    let wifi = with_metered_traffic(&netstats, 5_000_000, now, 10_101).replacen(
        "type=0, ratType=13, subscriberId=310260..., metered=true",
        "type=1, ratType=13, subscriberId=310260..., metered=true",
        1,
    );
    assert_eq!(usage(&wifi, now), []);
}

/// The alternatives of [`SIM_LOG_PATTERN`](spareline_core::SIM_LOG_PATTERN) as the literal text
/// each matches: `logcat -e` reads `[+]` as a plus sign.
const SIM_LOG_ALTERNATIVES: [&str; 8] = [
    "SIM_STATUS",
    "SIM_PLUG",
    "SIM_MISSING",
    "SIM_RECOVERY",
    "SIM_COMMON_SLOT",
    "SLOT_STATUS",
    "ICC_STATUS",
    "+ESIMS",
];

fn matches_sim_log_pattern(line: &str) -> bool {
    SIM_LOG_ALTERNATIVES
        .iter()
        .any(|alternative| line.contains(alternative))
}

#[test]
fn the_sim_log_alternatives_are_the_pattern() {
    let alternatives: Vec<String> = SIM_LOG_ALTERNATIVES
        .iter()
        .map(|alternative| match alternative.strip_prefix('+') {
            Some(rest) => format!("[+]{rest}"),
            None => (*alternative).to_owned(),
        })
        .collect();
    assert_eq!(alternatives.join("|"), spareline_core::SIM_LOG_PATTERN);
}

#[test]
fn the_sim_log_filter_ignores_the_b30s_usual_radio_log_traffic() {
    let log = fixture("radio-log.txt");
    assert!(log.contains("RILJ"), "the recording has RIL lines");
    let matching: Vec<&str> = log
        .lines()
        .filter(|line| matches_sim_log_pattern(line))
        .collect();
    assert_eq!(matching, Vec::<&str>::new());
}

#[test]
fn the_sim_log_filter_matches_sim_state_lines_in_the_rils_recorded_format() {
    for line in [
        "D/RILJ    ( 1937): [UNSL]< UNSOL_RESPONSE_SIM_STATUS_CHANGED [PHONE0]",
        "D/RILJ    ( 1937): [4801]> GET_SIM_STATUS [PHONE1]",
        "D/RILJ    ( 1937): [UNSL]< MTK: RIL_UNSOL_SIM_PLUG_OUT [PHONE1]",
        "D/RILJ    ( 1937): [UNSL]< MTK: RIL_UNSOL_SIM_PLUG_IN [PHONE1]",
        "D/RILJ    ( 1937): [UNSL]< MTK: RIL_UNSOL_SIM_MISSING [PHONE0]",
        "D/RILJ    ( 1937): [UNSL]< UNSOL_SIM_SLOT_STATUS_CHANGED",
        "I/AT      ( 1434): [1] AT< +ESIMS: 0,11 (RIL_URC2_READER, tid:1)",
    ] {
        assert!(matches_sim_log_pattern(line), "{line} matches");
        let mut scanner = SimLogScanner::default();
        assert!(
            scanner.push(format!("{line}\n").as_bytes()),
            "{line} signals"
        );
    }
}
