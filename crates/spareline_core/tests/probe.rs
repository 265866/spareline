//! The identity and health probes against output recorded from the NUU B30 (S6702X, Android 14)
//! with one SIM in slot 0. The identifiers in the recordings are fake.

use std::fs;
use std::path::Path;

use spareline_core::{PhoneIdentity, SimIdentity, SlotPresence, SlotState};

const B30_FINGERPRINT: &str = "NUU/S6702X1/S6702XA:14/UP1A.231005.007/1726207303:user/release-keys";

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/nuu-s6702x")
        .join(name);
    fs::read_to_string(&path).expect("the fixture is readable")
}

#[test]
fn the_commands_are_the_ones_spareline_old_sent() {
    assert_eq!(
        spareline_core::identity_command(),
        concat!(
            r"printf '\n@@sl:props\n'; for p in ro.product.manufacturer ro.product.model ",
            "ro.build.version.release ro.build.version.sdk ro.build.fingerprint ",
            "ro.build.version.security_patch persist.radio.multisim.config gsm.sim.state; ",
            r#"do echo "$p=$(getprop $p)"; done; printf '\n@@sl:isub\n'; dumpsys isub"#
        )
    );
    assert_eq!(
        spareline_core::health_command(),
        concat!(
            r"printf '\n@@sl:battery\n'; dumpsys battery; printf '\n@@sl:simstate\n'; ",
            r"getprop gsm.sim.state; printf '\n@@sl:registry\n'; dumpsys telephony.registry | ",
            r"grep -E '^  Phone Id=|^    m(ServiceState|SignalStrength|IsDataEnabled|",
            r"DataConnectionState|RadioPowerState)='"
        )
    );
}

#[test]
fn identity_of_the_b30_with_one_sim_in_slot_0() {
    let probe = spareline_core::parse_identity(&fixture("identity.txt"));
    let command = probe.subinfo_command().expect("SDK 34 has codes");
    assert_eq!(
        command,
        concat!(
            r"printf '\n@@sl:imei:0\n'; service call iphonesubinfo 4 i32 0 s16 com.android.shell s16 null; ",
            r"printf '\n@@sl:imei:1\n'; service call iphonesubinfo 4 i32 1 s16 com.android.shell s16 null; ",
            r"printf '\n@@sl:iccid:1\n'; service call iphonesubinfo 14 i32 1 s16 com.android.shell s16 null; ",
            r"printf '\n@@sl:number:1\n'; service call iphonesubinfo 16 i32 1 s16 com.android.shell s16 null"
        )
    );
    let identity = probe.apply_subinfo(&fixture("subscriber.txt"));
    assert_eq!(
        identity,
        PhoneIdentity {
            manufacturer: Some("NUU".to_owned()),
            model: Some("S6702X".to_owned()),
            android_version: Some("14".to_owned()),
            sdk: Some(34),
            fingerprint: Some(B30_FINGERPRINT.to_owned()),
            security_patch: Some("2024-09-05".to_owned()),
            imeis: vec![
                Some("350000000000006".to_owned()),
                Some("350000000000014".to_owned()),
            ],
            slots: vec![
                SlotState {
                    slot: 0,
                    state: SlotPresence::Present,
                    sim_state: Some("LOADED".to_owned()),
                },
                SlotState {
                    slot: 1,
                    state: SlotPresence::Empty,
                    sim_state: Some("ABSENT".to_owned()),
                },
            ],
            sims: vec![SimIdentity {
                slot: 0,
                sub_id: 1,
                iccid: Some("8901555000000000001".to_owned()),
                iccid_prefix: Some("890155500".to_owned()),
                carrier: Some("T-Mobile".to_owned()),
                country_iso: Some("us".to_owned()),
                mcc: Some("310".to_owned()),
                mnc: Some("240".to_owned()),
                number: Some("+15125550101".to_owned()),
            }],
            default_data_sub_id: Some(1),
            default_sms_sub_id: Some(1),
            sub_info_verified: true,
            warnings: Vec::new(),
        }
    );
}

#[test]
fn an_sdk_without_codes_reads_no_identifiers() {
    let output = fixture("identity.txt")
        .replace("ro.build.version.sdk=34", "ro.build.version.sdk=36")
        .replace("ro.build.version.release=14", "ro.build.version.release=16");
    let probe = spareline_core::parse_identity(&output);
    assert_eq!(probe.subinfo_command(), None);
    let identity = probe.into_identity();
    assert_eq!(identity.android_version.as_deref(), Some("16"));
    assert_eq!(identity.imeis, [None, None]);
    assert_eq!(identity.sims.len(), 1);
    assert_eq!(identity.sims[0].slot, 0);
    assert_eq!(identity.sims[0].iccid, None);
    assert_eq!(identity.sims[0].number, None);
    assert!(
        identity.warnings[0].contains("no iphonesubinfo codes for Android SDK 36"),
        "the warning names the SDK: {:?}",
        identity.warnings
    );
}

#[test]
fn another_sdk_34_build_uses_the_codes_unverified() {
    let output = fixture("identity.txt").replace(B30_FINGERPRINT, "OTHER/x:14/y");
    let probe = spareline_core::parse_identity(&output);
    let identity = probe.apply_subinfo(&fixture("subscriber.txt"));
    assert!(
        !identity.sub_info_verified,
        "another fingerprint is unverified"
    );
    assert_eq!(identity.imeis[0].as_deref(), Some("350000000000006"));
    assert_eq!(
        identity.warnings,
        ["iphonesubinfo codes for SDK 34 are unverified on this build"]
    );
}

#[test]
fn a_getter_value_that_is_not_an_imei_is_dropped_with_a_warning() {
    let recorded = fixture("subscriber.txt");
    let start = recorded.find("@@sl:imei:1\n").expect("an imei:1 section");
    let end = start + recorded[start..].find("')").expect("the end of its Parcel") + 2;
    let subscriber = format!(
        "{}@@sl:imei:1\nResult: Parcel(\t00000000 00000002 00320031   '....1.2.'){}",
        &recorded[..start],
        &recorded[end..]
    );
    let identity =
        spareline_core::parse_identity(&fixture("identity.txt")).apply_subinfo(&subscriber);
    assert_eq!(identity.imeis, [Some("350000000000006".to_owned()), None]);
    assert_eq!(
        identity.warnings,
        ["IMEI of slot 1: unexpected value shape, ignored"]
    );
}

#[test]
fn a_sim_still_loading_is_unknown_not_removed() {
    let output = fixture("identity.txt")
        .replace("mSimState[0]=LOADED", "mSimState[0]=NOT_READY")
        .replace(
            "gsm.sim.state=LOADED,ABSENT",
            "gsm.sim.state=NOT_READY,ABSENT",
        );
    let identity =
        spareline_core::parse_identity(&output).apply_subinfo(&fixture("subscriber.txt"));
    let states: Vec<SlotPresence> = identity.slots.iter().map(|slot| slot.state).collect();
    assert_eq!(states, [SlotPresence::Unknown, SlotPresence::Empty]);
    assert_eq!(identity.sims, []);
}

#[test]
fn health() {
    let health = spareline_core::parse_health(&fixture("health.txt"));
    let battery = health.battery.expect("a battery");
    assert_eq!(battery.level, 85);
    assert_eq!(battery.temperature_deci_c, 221);
    assert_eq!(health.sim_states, ["LOADED", "ABSENT"]);
    let radios: Vec<String> = health
        .radios
        .iter()
        .map(|radio| format!("{} {:?} {}", radio.slot, radio.voice, radio.signal_level))
        .collect();
    assert_eq!(radios, ["0 OutOfService 1", "1 PowerOff 0"]);
}
