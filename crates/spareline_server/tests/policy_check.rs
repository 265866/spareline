//! `policy-check` against a stand-in adb server that answers with a recorded read. The expected
//! rule lines are what spareline-old's `scripts/policy-check.ts` printed for the same read.

use std::future::Future;
use std::str;

use spareline_adb::{AdbClient, Endpoint};
use spareline_core::Timestamp;
use spareline_core::monitor::Policy;
use spareline_server::PolicyCheckError;
use tokio::net::{TcpListener, TcpStream};
use tokio::runtime::Builder;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

const RECORDED: &str =
    include_str!("../../spareline_core/tests/fixtures/nuu-s6702x/policy-read.txt");
const B30_PATH: &str = "../spareline_core/tests/fixtures/nuu-b30-no-data.json";
const B30: &str = include_str!("../../spareline_core/tests/fixtures/nuu-b30-no-data.json");
const NO_DATA_PATH: &str = "../spareline_core/tests/fixtures/no-data.json";
const NO_DATA: &str = include_str!("../../spareline_core/tests/fixtures/no-data.json");
const SERIAL: &str = "SPARELINE00001";

const B30_LINES: &str = "\
ok    mobile_data            mobile_data=0, mobile_data1=0
ok    data_roaming           data_roaming=0, data_roaming1=0
ok    wifi_off               0
ok    wifi_wakeup_off        0
ok    wifi_scan_off          0
ok    ble_scan_off           0
ok    bluetooth_off          0
ok    location_off           0
ok    ota_off                1
ok    stay_on_off            0
ok    brightness_manual      0
ok    brightness_low         10
ok    screen_timeout         15000
ok    data_saver             enabled
ok    sms_role               com.google.android.apps.messaging
ok    debloat                none installed
ok    mms_with_data_off      SIM 1 (sub 1) on
FAIL  log_buffer             256 KiB  (want logcat -g -b radio reports a ring buffer of 8 MiB or more)
ok    lock_screen_notifications_off 0
ok    lock_screen_notice     spareline-7d53969d19f0d42e
data watch: no internet mobile traffic in 24 h
";

const NO_DATA_LINES: &str = "\
ok    mobile_data            mobile_data=0, mobile_data1=0
ok    data_roaming           data_roaming=0, data_roaming1=0
ok    wifi_off               0
ok    wifi_wakeup_off        0
ok    wifi_scan_off          0
ok    ble_scan_off           0
ok    bluetooth_off          0
ok    location_off           0
ok    ota_off                1
ok    data_saver             enabled
ok    mms_with_data_off      SIM 1 (sub 1) on
data watch: no internet mobile traffic in 24 h
";

fn block_on<F: Future>(future: F) -> F::Output {
    Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime builds")
        .block_on(future)
}

async fn read_request(stream: &mut TcpStream) -> Option<String> {
    use tokio::io::AsyncReadExt as _;

    let mut length = [0; 4];
    stream.read_exact(&mut length).await.ok()?;
    let length = usize::from_str_radix(str::from_utf8(&length).ok()?, 16).ok()?;
    let mut body = vec![0; length];
    stream.read_exact(&mut body).await.ok()?;
    String::from_utf8(body).ok()
}

/// Answers one transport and shell request with `stdout` and exit code 0, and passes on every
/// request it was sent.
async fn serve(stream: &mut TcpStream, stdout: &[u8], requests: &UnboundedSender<String>) {
    use tokio::io::AsyncWriteExt as _;

    let Some(transport) = read_request(stream).await else {
        return;
    };
    requests
        .send(transport)
        .expect("the test collects the requests");
    stream.write_all(b"OKAY").await.expect("the client reads");
    let shell = read_request(stream).await.expect("a shell request");
    requests
        .send(shell)
        .expect("the test collects the requests");
    stream.write_all(b"OKAY").await.expect("the client reads");
    let mut packets = vec![1];
    let length = u32::try_from(stdout.len()).expect("the recorded read fits one packet");
    packets.extend_from_slice(&length.to_le_bytes());
    packets.extend_from_slice(stdout);
    packets.extend_from_slice(&[3, 1, 0, 0, 0, 0]);
    stream.write_all(&packets).await.expect("the client reads");
}

/// A running stand-in: the client that reaches it, and every request it was sent.
struct StandIn {
    adb: AdbClient,
    requests: UnboundedReceiver<String>,
}

impl StandIn {
    fn requests(&mut self) -> Vec<String> {
        let mut all = Vec::new();
        while let Ok(request) = self.requests.try_recv() {
            all.push(request);
        }
        all
    }
}

async fn stand_in(stdout: Vec<u8>) -> StandIn {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a loopback port is free");
    let port = listener
        .local_addr()
        .expect("the listener has an address")
        .port();
    let (sender, requests) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.expect("the client connects");
            serve(&mut stream, &stdout, &sender).await;
        }
    });
    StandIn {
        adb: AdbClient::new(Endpoint::new("127.0.0.1".to_owned(), port)),
        requests,
    }
}

/// Runs the check of `profile` on the stand-in's phone, and returns what it printed.
async fn check(
    stand_in_server: &StandIn,
    path: &str,
    profile: &str,
) -> Result<String, PolicyCheckError> {
    let mut out = Vec::new();
    spareline_server::policy_check(
        &stand_in_server.adb,
        SERIAL,
        path,
        profile,
        Timestamp::from_millis(1_792_659_282_000),
        &mut out,
    )
    .await?;
    Ok(String::from_utf8(out).expect("the report is UTF-8"))
}

/// Splits the report into its read line and the rest.
fn split_read_line(report: &str) -> [&str; 2] {
    let (read, rest) = report
        .split_once('\n')
        .expect("the report starts with the read line");
    [read, rest]
}

/// Asserts the read line names one shell call, the bytes read and exit 0.
fn assert_read_line(read: &str, bytes: usize) {
    let rest = read
        .strip_prefix("read: 1 shell call, ")
        .unwrap_or_else(|| panic!("{read}"));
    let (millis, tail) = rest.split_once(" ms, ").unwrap_or_else(|| panic!("{read}"));
    assert!(millis.parse::<u64>().is_ok(), "{read}");
    assert_eq!(tail, format!("{bytes} bytes, exit 0"), "{read}");
}

#[test]
fn prints_every_rule_as_spareline_old_did_and_runs_only_the_read() {
    block_on(async {
        let mut stand_in_server = stand_in(RECORDED.as_bytes().to_vec()).await;
        let report = check(&stand_in_server, B30_PATH, B30).await.unwrap();
        let [read, rest] = split_read_line(&report);
        assert_read_line(read, RECORDED.len());
        assert_eq!(rest, B30_LINES);
        let read_script = Policy::load(B30_PATH, B30).unwrap().read_script;
        assert_eq!(
            stand_in_server.requests(),
            [
                format!("host:transport:{SERIAL}"),
                format!("shell,v2,raw:{read_script}")
            ],
            "the check runs the profile's read and nothing else"
        );
    });
}

#[test]
fn a_profile_without_device_rules_prints_fewer_rules() {
    block_on(async {
        let stand_in_server = stand_in(RECORDED.as_bytes().to_vec()).await;
        let report = check(&stand_in_server, NO_DATA_PATH, NO_DATA)
            .await
            .unwrap();
        assert_eq!(split_read_line(&report)[1], NO_DATA_LINES);
    });
}

#[test]
fn a_leading_byte_order_mark_is_dropped_before_parsing_as_text_decoder_did() {
    block_on(async {
        let read = RECORDED.trim_start_matches('\n');
        let mut stdout = "\u{feff}".as_bytes().to_vec();
        stdout.extend_from_slice(read.as_bytes());
        let stand_in_server = stand_in(stdout.clone()).await;
        let report = check(&stand_in_server, B30_PATH, B30).await.unwrap();
        let [read_line, rest] = split_read_line(&report);
        assert_read_line(read_line, stdout.len());
        assert_eq!(
            rest, B30_LINES,
            "with the mark left in, the first section's marker isn't recognized"
        );
    });
}

/// The recorded read with mobile traffic on two SIMs in the last day: 5000 bytes received and
/// 700 sent on sub 1, and 50 received on sub 2.
fn with_traffic() -> String {
    let now = 1_792_659_282;
    let row = |sub: u8, uid: u32, received: u32, sent: u32| {
        format!(
            "  ident=[{{type=0, ratType=13, subscriberId=310260..., metered=true, \
             defaultNetwork=true, oemManaged=OEM_NONE, subId={sub}}}] uid={uid} set=DEFAULT \
             tag=0x0\n    NetworkStatsHistory: bucketDuration=7200\n      st={} rb={received} \
             rp=900 tb={sent} tp=0 op=0\n",
            now - 3600
        )
    };
    let index = RECORDED
        .find("\nInterfaceMapUpdater:")
        .expect("the recorded netstats has an interface map")
        + 1;
    format!(
        "{}{}{}{}",
        &RECORDED[..index],
        row(1, 10_123, 5000, 700),
        row(2, 10_124, 50, 0),
        &RECORDED[index..]
    )
}

#[test]
fn the_data_watch_sums_each_sims_traffic_as_spareline_old_did() {
    block_on(async {
        let stand_in_server = stand_in(with_traffic().into_bytes()).await;
        let report = check(&stand_in_server, NO_DATA_PATH, NO_DATA)
            .await
            .unwrap();
        assert_eq!(
            report.lines().last(),
            Some("data watch: sub 1: 5700 bytes, sub 2: 50 bytes")
        );
    });
}

#[test]
fn an_invalid_profile_fails_before_reaching_the_phone() {
    block_on(async {
        let mut stand_in_server = stand_in(RECORDED.as_bytes().to_vec()).await;
        let error = check(&stand_in_server, "broken.json", "{")
            .await
            .unwrap_err();
        assert!(matches!(error, PolicyCheckError::Profile(_)), "{error:?}");
        assert_eq!(
            stand_in_server.requests(),
            Vec::<String>::new(),
            "an invalid profile never reaches the adb server"
        );
    });
}

#[test]
fn an_unreachable_adb_server_is_an_error() {
    block_on(async {
        let adb = AdbClient::new(Endpoint::new("127.0.0.1".to_owned(), 1));
        let mut out = Vec::new();
        let error = spareline_server::policy_check(
            &adb,
            SERIAL,
            B30_PATH,
            B30,
            Timestamp::from_millis(0),
            &mut out,
        )
        .await
        .unwrap_err();
        assert!(
            error
                .to_string()
                .starts_with("cannot reach the adb server at 127.0.0.1:1: "),
            "{error}"
        );
        assert_eq!(
            out,
            Vec::<u8>::new(),
            "nothing is printed before the read answers"
        );
    });
}
