//! The Android output parsers against output recorded from a NUU B30 (S6702X, Android 14) with
//! one T-Mobile SIM in slot 0 and slot 1 empty, out of service where it was recorded. The
//! identifiers in the recordings are fake; `fixtures/nuu-s6702x/meta.json` says what else changed.

use std::fs;
use std::path::Path;

use spareline_core::{
    Battery, BatteryHealth, BatteryStatus, ParcelString, PowerSource, RadioState, ServiceState,
    SubscriptionRecord,
};

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/nuu-s6702x")
        .join(name);
    fs::read_to_string(&path).expect("the fixture is readable")
}

fn section(file: &str, name: &str) -> String {
    spareline_core::split_sections(&fixture(file))
        .get(name)
        .unwrap_or_else(|| panic!("{file} has no section {name}"))
        .to_owned()
}

#[test]
fn props() {
    let props = spareline_core::parse_key_values(&section("identity.txt", "props"));
    let prop = |key: &str| props.get(key).map(String::as_str);
    assert_eq!(prop("ro.product.model"), Some("S6702X"));
    assert_eq!(prop("ro.build.version.release"), Some("14"));
    assert_eq!(prop("ro.build.version.sdk"), Some("34"));
    assert_eq!(prop("gsm.sim.state"), Some("LOADED,ABSENT"));
    assert_eq!(
        spareline_core::parse_sim_states(prop("gsm.sim.state").unwrap_or("")),
        ["LOADED", "ABSENT"]
    );
}

#[test]
fn dumpsys_battery() {
    assert_eq!(
        spareline_core::parse_battery(&section("health.txt", "battery")),
        Some(Battery {
            level: 85,
            temperature_deci_c: 221,
            voltage_mv: Some(4103),
            status: BatteryStatus::NotCharging,
            health: BatteryHealth::Good,
            plugged: vec![PowerSource::Usb],
        })
    );
}

#[test]
fn dumpsys_isub() {
    let isub = spareline_core::parse_isub(&section("identity.txt", "isub"));
    assert_eq!(isub.active_modem_count, Some(2));
    assert_eq!(isub.slot_sub_ids.into_iter().collect::<Vec<_>>(), [(0, 1)]);
    assert_eq!(
        isub.sim_states.into_iter().collect::<Vec<_>>(),
        [(0, "LOADED".to_owned()), (1, "ABSENT".to_owned())]
    );
    assert_eq!(isub.all_loaded, Some(true));
    assert_eq!(isub.default_data_sub_id, Some(1));
    assert_eq!(isub.default_sms_sub_id, Some(1));
    assert_eq!(
        isub.active,
        [SubscriptionRecord {
            sub_id: 1,
            slot: 0,
            iccid_prefix: Some("890155500".to_owned()),
            display_name: Some("T-Mobile".to_owned()),
            carrier_name: Some("Emergency calls only".to_owned()),
            mcc: Some("310".to_owned()),
            mnc: Some("240".to_owned()),
            country_iso: Some("us".to_owned()),
        }],
        "carrierName reflects service, and values may contain spaces"
    );
}

#[test]
fn registry_out_of_service_with_slot_1_empty() {
    let states = spareline_core::parse_registry(&section("health.txt", "registry"));
    assert_eq!(
        states,
        [
            RadioState {
                slot: 0,
                voice: ServiceState::OutOfService,
                data: ServiceState::OutOfService,
                operator: None,
                emergency_only: true,
                signal_level: 1,
                signal_type: Some("LTE".to_owned()),
                signal_dbm: Some(-122),
                data_enabled: Some(false),
                data_connected: Some(false),
            },
            RadioState {
                slot: 1,
                voice: ServiceState::PowerOff,
                data: ServiceState::PowerOff,
                operator: None,
                emergency_only: false,
                signal_level: 0,
                signal_type: None,
                signal_dbm: None,
                data_enabled: Some(false),
                data_connected: None,
            },
        ]
    );
}

#[test]
fn registry_in_service() {
    let states = spareline_core::parse_registry(&section("health-in-service.txt", "registry"));
    let slot0 = &states[0];
    assert_eq!(slot0.voice, ServiceState::InService);
    assert_eq!(slot0.data, ServiceState::InService);
    assert_eq!(slot0.operator.as_deref(), Some("T-Mobile"));
    assert!(!slot0.emergency_only, "in service is not emergency only");
}

#[test]
fn radio_state_serializes_as_the_api_radio() {
    let states = spareline_core::parse_registry(&section("health.txt", "registry"));
    assert_eq!(
        serde_json::to_string(&states).expect("serializes"),
        concat!(
            r#"[{"slot":0,"voice":"out_of_service","data":"out_of_service","operator":null,"#,
            r#""emergencyOnly":true,"signalLevel":1,"signalType":"LTE","signalDbm":-122,"#,
            r#""dataEnabled":false,"dataConnected":false},"#,
            r#"{"slot":1,"voice":"power_off","data":"power_off","operator":null,"#,
            r#""emergencyOnly":false,"signalLevel":0,"signalType":null,"signalDbm":null,"#,
            r#""dataEnabled":false,"dataConnected":null}]"#
        )
    );
}

#[test]
fn parcel_strings_over_several_lines() {
    let value = |name: &str| spareline_core::parse_parcel_string(&section("subscriber.txt", name));
    let string = |text: &str| ParcelString::Value(Some(text.to_owned()));
    assert_eq!(value("imei:0"), string("350000000000006"));
    assert_eq!(value("imei:1"), string("350000000000014"));
    assert_eq!(value("iccid:1"), string("8901555000000000001"));
    assert_eq!(value("number:1"), string("15125550101"));
}

#[test]
fn parcel_tool_errors() {
    let sections = spareline_core::split_sections(&fixture("service-call-errors.txt"));
    let parse = |name: &str| spareline_core::parse_parcel_string(sections.get(name).unwrap_or(""));
    assert_eq!(
        parse("unknown-code"),
        ParcelString::Invalid {
            reason: r#"Error: 0xffffffffffffffb6 "Not a data message""#.to_owned(),
        }
    );
    assert_eq!(
        parse("no-service"),
        ParcelString::Invalid {
            reason: "service: Service nosuchservice does not exist".to_owned(),
        }
    );
}
