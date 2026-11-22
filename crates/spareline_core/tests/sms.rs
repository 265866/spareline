//! Reading texts against `dumpsys isms` and provider output recorded from the NUU B30. The senders,
//! bodies and message ids in the recordings are fake.

use std::fs;
use std::path::Path;

use spareline_core::{IsmsActivity, Provider, ProviderQuery, Timestamp};

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/nuu-s6702x")
        .join(name);
    fs::read_to_string(&path).expect("the fixture is readable")
}

#[test]
fn the_recorded_inbox_every_row_and_bodies_decode_as_utf8() {
    let query = ProviderQuery::After {
        provider: Provider::Sms,
        after_id: 0,
    };
    let parsed =
        spareline_core::check_content_output(&fixture("sms-provider.txt"), "", 0, query.columns())
            .expect("the recording passes the check");
    assert_eq!(parsed.malformed, Vec::<String>::new());
    let ids: Vec<&str> = parsed
        .rows
        .iter()
        .map(|row| row.get("_id").unwrap_or(""))
        .collect();
    assert_eq!(ids, ["4", "5", "6", "7", "8", "9", "10", "11"]);
    let first = &parsed.rows[0];
    assert_eq!(first.get("thread_id"), Some("2"));
    assert_eq!(first.get("address"), Some("550"));
    assert_eq!(first.get("date"), Some("1792637380314"));
    assert_eq!(first.get("date_sent"), Some("1792623882000"));
    assert_eq!(first.get("sub_id"), Some("1"));
    assert_eq!(first.get("type"), Some("1"));
    assert_eq!(parsed.rows[1].get("address"), Some("+15125550151"));
    let messages: Vec<_> = parsed
        .rows
        .iter()
        .map(|row| spareline_core::sms_message(row).expect("every row is a message"))
        .collect();
    assert_eq!(
        messages[7].body,
        "Fake text 8, is the café table still for sale? ✓"
    );
    assert_eq!(
        messages[0].received_at.to_string(),
        "2026-10-22T02:49:40.314Z"
    );
    assert_eq!(
        messages[0].sent_at.map(|at| at.to_string()).as_deref(),
        Some("2026-10-21T23:04:42.000Z")
    );
}

#[test]
fn the_recorded_inbox_keys() {
    let query = ProviderQuery::Keys {
        provider: Provider::Sms,
    };
    let parsed =
        spareline_core::check_content_output(&fixture("sms-keys.txt"), "", 0, query.columns())
            .expect("the recording passes the check");
    let keys = spareline_core::inbox_keys(&parsed.rows, Provider::Sms);
    assert_eq!(keys.len(), 8);
    assert_eq!(keys[0].provider_id, 4);
    assert_eq!(
        keys[0].received_at,
        Timestamp::from_millis(1_792_637_380_314)
    );
}

#[test]
fn the_recorded_mms_inbox_is_empty() {
    for (file, query) in [
        (
            "mms-provider.txt",
            ProviderQuery::After {
                provider: Provider::Mms,
                after_id: 0,
            },
        ),
        (
            "mms-keys.txt",
            ProviderQuery::Keys {
                provider: Provider::Mms,
            },
        ),
    ] {
        let parsed = spareline_core::check_content_output(&fixture(file), "", 0, query.columns())
            .expect("the recording passes the check");
        assert_eq!(parsed.rows.len(), 0, "{file} has no rows");
    }
}

#[test]
fn the_recorded_trigger_output() {
    let activity = spareline_core::parse_isms(&fixture("isms.txt"));
    assert_eq!(
        activity,
        IsmsActivity {
            handler_records: vec![105, 2, 105, 2],
            last_event_at: Some("10-21 19:50:27.278".to_owned()),
            deliveries: 16,
            last_delivery_at: Some("2026-10-21T19:50:27.267498".to_owned()),
        },
        "GSM and CDMA handlers, printed once for sub 1 and once for sub -1"
    );
}

#[test]
fn a_new_inbound_sms_changes_the_activity_and_a_reread_does_not() {
    let before = fixture("isms.txt");
    let after = before
        .replacen("total records=105", "total records=111", 1)
        .replacen(
            "      CdmaInboundSmsHandler:",
            "       rec[19]: time=10-22 08:01:02.003 processed=MtkIdleState org=MtkIdleState \
         dest=MtkDeliveringState what=EVENT_NEW_SMS\n      CdmaInboundSmsHandler:",
            1,
        );
    let first = spareline_core::parse_isms(&before);
    assert_eq!(spareline_core::parse_isms(&before), first);
    let next = spareline_core::parse_isms(&after);
    assert_eq!(next.last_event_at.as_deref(), Some("10-22 08:01:02.003"));
    assert_ne!(next, first);
}

#[test]
fn the_trigger_command_is_the_one_spareline_old_sent() {
    assert_eq!(
        spareline_core::ISMS_TRIGGER_COMMAND,
        "dumpsys isms | grep -E 'InboundSmsHandler:$|total records=|what=EVENT_(NEW|BROADCAST)_SMS|Delivering SMS to'"
    );
}
