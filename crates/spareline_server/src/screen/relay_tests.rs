//! spareline-old's `src/screens/manager.test.ts`, against a stand-in phone behind a stand-in adb
//! server, on tokio's paused clock.

use std::pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures_util::future;

use serde_json::{Value, json};
use spareline_core::screen::{DISPLAY_POWER_OFF, RESET_VIDEO};
use tokio::time::{self, Instant};

use crate::clock::Clock;
use crate::fake_adb::{self, B30};
use crate::screen::fake_scrcpy::{self, Orphan};
use crate::screen::screen_harness::{self, Harness, WAKE, Watcher};
use crate::screen::screen_session::ScreenState;
use crate::screen::screen_timings::ScreenTimings;
use crate::screen::viewer_close::ViewerClose;

/// The keyframe requests of one session are at least this far apart, so tests that count them
/// wait this long between steps.
const RESET_SPACING: Duration = Duration::from_millis(600);

/// How long each pkill takes in the tests of when the monitor unlists a phone, so a session's
/// teardown takes a while after the relay drops it.
const KILL_DELAY: Duration = Duration::from_millis(500);

/// Waits for a session's own pkill, checking that the phone stays listed in the monitor's
/// screen sessions while it runs and is unlisted only once it finished: until then scrcpy may
/// still hold `stay_on_while_plugged_in`.
async fn listed_until_killed(h: &Harness) {
    let scid_kills_done = || {
        h.phone
            .kills_done()
            .iter()
            .filter(|kill| kill.contains("scid="))
            .count()
    };
    let done = scid_kills_done();
    fake_adb::until("the session's own pkill", || h.scid_kills() > done).await;
    assert_eq!(scid_kills_done(), done, "the pkill is still running");
    assert!(
        h.monitor_sessions.is_active(B30),
        "listed while the pkill runs"
    );
    fake_adb::until("unlisted", || !h.monitor_sessions.is_active(B30)).await;
    assert!(
        scid_kills_done() > done,
        "unlisted only after the pkill finished"
    );
}

/// A viewer got a hello before any packet, and a config packet first.
fn assert_hello_first(viewer: &Watcher) {
    let hello = viewer.order.iter().position(|kind| kind == "hello");
    let binary = viewer.order.iter().position(|kind| kind == "binary");
    assert!(hello.is_some(), "a hello came: {:?}", viewer.order);
    assert!(
        binary > hello,
        "packets came after the hello: {:?}",
        viewer.order
    );
    assert!(
        fake_scrcpy::is_config(&viewer.binary[0]),
        "the first packet is a config packet"
    );
}

fn count(log: &[String], entry: &str) -> usize {
    log.iter().filter(|logged| *logged == entry).count()
}

fn position(log: &[String], entry: &str) -> Option<usize> {
    log.iter().position(|logged| logged == entry)
}

async fn settle_resets() {
    time::sleep(RESET_SPACING).await;
}

#[test]
fn viewers_of_one_phone_share_one_session_and_each_gets_hello_then_a_fresh_config_and_keyframe() {
    fake_adb::block_on(async {
        let h = Harness::start(screen_harness::test_timings()).await;
        let mut first = h.watch("someone").await;
        fake_adb::until("the first viewer's packets", || {
            first.pump();
            first.binary.len() >= 2
        })
        .await;
        assert_eq!(
            first.hellos(),
            vec![json!({"t": "hello", "width": 216, "height": 480})]
        );
        assert_hello_first(&first);

        settle_resets().await;
        let mut second = h.watch("someone").await;
        fake_adb::until("the second viewer's packets", || {
            second.pump();
            second.binary.len() >= 2
        })
        .await;
        assert_hello_first(&second);
        assert_eq!(h.phone.servers().len(), 1, "one server for both viewers");
        assert_eq!(h.phone.pushes(), 1);
        fake_adb::until("the joiner's second keyframe request", || {
            let control = &h.phone.servers()[0].control;
            control
                .iter()
                .filter(|m| m.as_slice() == RESET_VIDEO)
                .count()
                >= 3
        })
        .await;
        settle_resets().await;
        let control = h.phone.servers()[0].control.clone();
        assert!(
            control.contains(&DISPLAY_POWER_OFF.to_vec()),
            "the panel went dark"
        );
        assert_eq!(
            control
                .iter()
                .filter(|m| m.as_slice() == RESET_VIDEO)
                .count(),
            3,
            "one keyframe request after the start's wake, and the joiner's at once and after \
             its wake"
        );
        let list = h.screens.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].serial, B30);
        assert_eq!(list[0].state, ScreenState::Running);
        assert_eq!(list[0].viewers, 2);
        h.screens.shutdown().await;
    });
}

#[test]
fn a_late_joiners_first_packet_is_a_config_packet_even_when_the_encoder_sends_a_keyframe_first() {
    fake_adb::block_on(async {
        let h = Harness::start(screen_harness::test_timings()).await;
        let mut first = h.watch("someone").await;
        fake_adb::until("the first viewer's packets", || {
            first.pump();
            first.binary.len() >= 2
        })
        .await;
        h.phone.set_answer_resets(false);
        let mut second = h.watch("someone").await;
        fake_adb::until("the joiner's hello", || {
            second.pump();
            second.hellos().len() == 1
        })
        .await;
        let before = first.binary.len();
        h.phone.send_keyframe(0);
        fake_adb::until("the keyframe reaches the first viewer", || {
            first.pump();
            first.binary.len() > before
        })
        .await;
        second.pump();
        assert!(
            second.binary.is_empty(),
            "no keyframe before a config packet"
        );

        h.phone.answer_reset(0);
        fake_adb::until("the joiner's packets", || {
            second.pump();
            second.binary.len() >= 2
        })
        .await;
        assert!(fake_scrcpy::is_config(&second.binary[0]));
        assert!(fake_scrcpy::is_keyframe(&second.binary[1]));
        h.screens.shutdown().await;
    });
}

#[test]
fn the_phone_is_woken_and_its_keyguard_dismissed_before_the_panel_goes_dark_at_start_and_at_each_join()
 {
    fake_adb::block_on(async {
        let h = Harness::start(screen_harness::test_timings()).await;
        let _first = h.watch("someone").await;
        fake_adb::until("the panel goes dark", || {
            h.phone.log().contains(&"control:10".to_owned())
        })
        .await;
        let log = h.phone.log();
        let woke = position(&log, WAKE).expect("the phone was woken");
        assert!(
            position(&log[..woke], "control:10").is_none(),
            "nothing darkened before the wake"
        );
        let after = &log[woke..];
        assert!(
            position(after, "control:17").is_some(),
            "a keyframe after the wake"
        );
        assert!(position(after, "control:10") > position(after, "control:17"));

        settle_resets().await;
        let before = h.phone.log().len();
        let _second = h.watch("someone").await;
        fake_adb::until("the joiner's wake, keyframe and dark panel", || {
            let log = h.phone.log();
            let Some(woke) = position(&log[before..], WAKE) else {
                return false;
            };
            let after = &log[before + woke..];
            after.contains(&"control:17".to_owned()) && after.contains(&"control:10".to_owned())
        })
        .await;
        let log = h.phone.log();
        let woke = before + position(&log[before..], WAKE).unwrap();
        assert!(
            position(&log[before..woke], "control:10").is_none(),
            "the joiner's wake came before the panel went dark again"
        );
        assert_eq!(h.phone.servers().len(), 1);
        h.screens.shutdown().await;
    });
}

#[test]
fn a_phone_that_falls_asleep_while_watched_is_woken_once_and_an_awake_one_is_left_alone() {
    fake_adb::block_on(async {
        let h = Harness::start(ScreenTimings {
            wake_check: Duration::from_millis(20),
            ..screen_harness::test_timings()
        })
        .await;
        let viewer = h.watch("someone").await;
        fake_adb::until("the panel goes dark", || {
            h.phone.log().contains(&"control:10".to_owned())
        })
        .await;
        let reads = h.phone.wakefulness_reads();
        fake_adb::until("three wake checks", || {
            h.phone.wakefulness_reads() >= reads + 3
        })
        .await;
        assert_eq!(h.wakes(), 1, "an awake phone isn't woken");

        settle_resets().await;
        h.phone.set_wakefulness("Dozing");
        let before = h.phone.log().len();
        fake_adb::until("the dozing phone is woken and darkened", || {
            h.phone.log()[before..].contains(&"control:10".to_owned())
        })
        .await;
        let log = h.phone.log();
        let after = &log[before..];
        assert_eq!(count(after, WAKE), 1);
        assert!(position(after, WAKE) < position(after, "control:17"));
        assert!(position(after, "control:17") < position(after, "control:10"));
        time::sleep(Duration::from_millis(100)).await;
        assert_eq!(h.wakes(), 2, "woken once");

        viewer.detach();
        fake_adb::until("the session stops", || h.screens.list().is_empty()).await;
        let stopped = h.phone.wakefulness_reads();
        time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            h.phone.wakefulness_reads(),
            stopped,
            "no checks once the session stopped"
        );
        h.screens.shutdown().await;
    });
}

#[test]
fn a_joiner_gets_a_keyframe_at_once_before_the_wake_finishes() {
    fake_adb::block_on(async {
        let h = Harness::start(screen_harness::test_timings()).await;
        let _first = h.watch("someone").await;
        fake_adb::until("the panel goes dark", || {
            h.phone.log().contains(&"control:10".to_owned())
        })
        .await;
        settle_resets().await;
        let before = h.phone.log().len();
        let _second = h.watch("someone").await;
        fake_adb::until("the joiner's dark panel", || {
            h.phone.log()[before..].contains(&"control:10".to_owned())
        })
        .await;
        assert_eq!(h.phone.log()[before], "control:17");
        h.screens.shutdown().await;
    });
}

#[test]
fn nobody_watching_during_the_idle_window_leaves_the_phone_to_sleep() {
    fake_adb::block_on(async {
        let h = Harness::start(ScreenTimings {
            wake_check: Duration::from_millis(20),
            idle: Duration::from_millis(400),
            ..screen_harness::test_timings()
        })
        .await;
        let viewer = h.watch("someone").await;
        fake_adb::until("the panel goes dark", || {
            h.phone.log().contains(&"control:10".to_owned())
        })
        .await;
        fake_adb::until("two wake checks", || h.phone.wakefulness_reads() >= 2).await;
        viewer.detach();
        h.phone.set_wakefulness("Dozing");
        fake_adb::until("the viewer has left", || h.screens.list()[0].viewers == 0).await;
        let reads = h.phone.wakefulness_reads();
        time::sleep(Duration::from_millis(200)).await;
        assert_eq!(h.screens.list()[0].state, ScreenState::Running);
        assert_eq!(h.wakes(), 1);
        assert_eq!(
            h.phone.wakefulness_reads(),
            reads,
            "no checks while unwatched"
        );
        assert_eq!(h.events("screen-woke"), Vec::<Value>::new());
        h.screens.shutdown().await;
    });
}

#[test]
fn after_a_wake_the_next_check_darkens_the_panel_once_more() {
    fake_adb::block_on(async {
        let h = Harness::start(ScreenTimings {
            wake_check: Duration::from_millis(20),
            ..screen_harness::test_timings()
        })
        .await;
        let _viewer = h.watch("someone").await;
        fake_adb::until("the panel goes dark", || {
            h.phone.log().contains(&"control:10".to_owned())
        })
        .await;
        h.phone.set_wakefulness("Dozing");
        let before = h.phone.log().len();
        fake_adb::until("the panel is darkened twice", || {
            count(&h.phone.log()[before..], "control:10") >= 2
        })
        .await;
        assert_eq!(count(&h.phone.log()[before..], WAKE), 1);
        time::sleep(Duration::from_millis(150)).await;
        assert_eq!(
            count(&h.phone.log()[before..], "control:10"),
            2,
            "later checks find the phone awake and write nothing"
        );
        h.screens.shutdown().await;
    });
}

#[test]
fn a_wake_that_does_not_stick_is_retried_logged_once_then_warned_about_every_6th_check() {
    fake_adb::block_on(async {
        let h = Harness::start(ScreenTimings {
            wake_check: Duration::from_millis(10),
            ..screen_harness::test_timings()
        })
        .await;
        let _viewer = h.watch("someone").await;
        fake_adb::until("the panel goes dark", || {
            h.phone.log().contains(&"control:10".to_owned())
        })
        .await;
        h.phone.set_wake_sticks(false);
        h.phone.set_wakefulness("Dozing");
        fake_adb::until("13 wakes", || h.wakes() >= 13).await;
        let woke = h.events("screen-woke");
        assert_eq!(woke.len(), 1);
        assert_eq!(woke[0]["was"], "Dozing");
        assert_eq!(woke[0]["serial"], B30);
        let warned = h.events("screen-wake-not-sticking");
        assert!(warned.len() >= 2, "{warned:?}");
        assert_eq!(warned[0]["checks"], 6);
        assert_eq!(warned[0]["state"], "Dozing");
        assert_eq!(warned[1]["checks"], 12);

        h.phone.set_wake_sticks(true);
        fake_adb::until("the phone is awake again", || {
            h.phone.wakefulness() == "Awake"
        })
        .await;
        time::sleep(Duration::from_millis(50)).await;
        h.phone.set_wakefulness("Dozing");
        fake_adb::until("a second screen-woke", || {
            h.events("screen-woke").len() == 2
        })
        .await;
        h.screens.shutdown().await;
    });
}

#[test]
fn a_failed_wake_is_logged_and_the_session_carries_on_with_a_keyframe_and_the_panel_dark() {
    fake_adb::block_on(async {
        let h = Harness::start(screen_harness::test_timings()).await;
        h.phone.set_wake_fails(true);
        let mut viewer = h.watch("someone").await;
        fake_adb::until("the panel goes dark", || {
            h.phone.log().contains(&"control:10".to_owned())
        })
        .await;
        assert_eq!(h.events("screen-wake-failed").len(), 1);
        let log = h.phone.log();
        let failed = position(&log, WAKE);
        assert!(position(&log, "control:17") > failed);
        assert!(position(&log, "control:10") > failed);
        assert_eq!(h.screens.list()[0].state, ScreenState::Running);
        fake_adb::until("packets", || {
            viewer.pump();
            viewer.binary.len() >= 2
        })
        .await;
        assert_eq!(viewer.closed, None);
        h.screens.shutdown().await;
    });
}

#[test]
fn a_session_stops_only_after_the_last_viewer_has_been_gone_for_the_idle_time() {
    fake_adb::block_on(async {
        let h = Harness::start(ScreenTimings {
            idle: Duration::from_millis(150),
            ..screen_harness::test_timings()
        })
        .await;
        let a = h.watch("someone").await;
        let b = h.watch("someone").await;
        fake_adb::until("the session runs", || {
            h.screens.list()[0].state == ScreenState::Running
        })
        .await;
        a.detach();
        b.detach();
        time::sleep(Duration::from_millis(75)).await;
        assert!(h.phone.servers()[0].alive, "still within the idle time");

        let mut c = h.watch("someone").await;
        time::sleep(Duration::from_millis(150)).await;
        c.pump();
        assert_eq!(
            h.phone.servers().len(),
            1,
            "coming back keeps the same server"
        );
        assert_eq!(c.hellos().len(), 1);

        c.detach();
        fake_adb::until("the server is killed", || !h.phone.servers()[0].alive).await;
        fake_adb::until("the entry goes", || h.screens.list().is_empty()).await;
        assert_eq!(h.scid_kills(), 1);
        h.screens.shutdown().await;
    });
}

#[test]
fn a_viewer_that_leaves_and_one_that_joins_while_a_start_is_in_flight_share_that_start() {
    fake_adb::block_on(async {
        let h = Harness::start(screen_harness::test_timings()).await;
        h.phone.hold_starts();
        let first = h.watch("someone").await;
        fake_adb::until("the server runs", || h.phone.servers().len() == 1).await;
        first.detach();
        let mut second = h.watch("someone").await;
        h.phone.release_starts();
        fake_adb::until("the joiner's packets", || {
            second.pump();
            second.binary.len() >= 2
        })
        .await;
        assert_hello_first(&second);
        assert_eq!(h.phone.servers().len(), 1);
        h.screens.shutdown().await;
    });
}

#[test]
fn the_last_viewer_leaving_during_a_start_stops_the_session_after_the_idle_time() {
    fake_adb::block_on(async {
        let h = Harness::start(ScreenTimings {
            idle: Duration::from_millis(50),
            ..screen_harness::test_timings()
        })
        .await;
        h.phone.hold_starts();
        let viewer = h.watch("someone").await;
        fake_adb::until("the server runs", || h.phone.servers().len() == 1).await;
        viewer.detach();
        h.phone.release_starts();
        fake_adb::until("the entry goes", || h.screens.list().is_empty()).await;
        assert!(!h.phone.servers()[0].alive);
        assert_eq!(h.phone.servers().len(), 1);
        h.screens.shutdown().await;
    });
}

#[test]
fn shutdown_returns_only_after_every_server_has_been_killed_including_ones_with_viewers() {
    fake_adb::block_on(async {
        let h = Harness::start(screen_harness::test_timings()).await;
        let mut viewer = h.watch("someone").await;
        fake_adb::until("the hello", || {
            viewer.pump();
            viewer.hellos().len() == 1
        })
        .await;
        h.phone.set_kill_delay(Duration::from_millis(50));
        h.screens.shutdown().await;
        let done = h.phone.kills_done();
        assert_eq!(done.iter().filter(|kill| kill.contains("scid=")).count(), 1);
        viewer.pump();
        assert_eq!(viewer.closed.map(|close| close.code), Some(1001));
        assert_eq!(h.audits(), vec!["someone 0 0 0 closed".to_owned()]);
    });
}

#[test]
fn a_start_kills_an_orphan_server_and_waits_for_its_cleanup_before_pushing_the_jar() {
    fake_adb::block_on(async {
        let h = Harness::start(screen_harness::test_timings()).await;
        h.phone.set_orphan(Orphan {
            unkillable: false,
            cleanup: Duration::from_millis(150),
        });
        let began = Instant::now();
        let mut viewer = h.watch("someone").await;
        fake_adb::until("the push", || h.phone.pushes() == 1).await;
        assert!(
            began.elapsed() >= Duration::from_millis(140),
            "{:?}",
            began.elapsed()
        );
        assert_eq!(
            h.phone.kills()[0],
            "pkill -f 'com.genymobile.scrcpy.Serve[r]' || true"
        );
        fake_adb::until("the hello", || {
            viewer.pump();
            viewer.hellos().len() == 1
        })
        .await;
        let started = h.events("screen-session-started");
        assert_eq!(started.len(), 1);
        assert!(
            started[0]["settleMs"].as_u64().unwrap() >= 140,
            "{started:?}"
        );
        h.screens.shutdown().await;
    });
}

#[test]
fn a_start_fails_when_scrcpy_processes_do_not_go_away() {
    fake_adb::block_on(async {
        let h = Harness::start(ScreenTimings {
            restart_delays: Vec::new(),
            ..screen_harness::test_timings()
        })
        .await;
        h.phone.set_orphan(Orphan {
            unkillable: true,
            cleanup: Duration::ZERO,
        });
        let mut viewer = h.watch("someone").await;
        fake_adb::until("the viewer is closed", || {
            viewer.pump();
            viewer.closed.is_some()
        })
        .await;
        let closed = viewer.closed.unwrap();
        assert_eq!(closed.code, 4002);
        assert_eq!(
            closed.reason,
            "screen session failed: scrcpy processes 4242 still running after 3000 ms"
        );
        assert_eq!(h.phone.pushes(), 0);
        assert!(h.phone.servers().is_empty());
        h.screens.shutdown().await;
    });
}

#[test]
fn allowed_input_is_forwarded_and_audited_and_the_rest_is_dropped_and_counted() {
    fake_adb::block_on(async {
        let h = Harness::start(screen_harness::test_timings()).await;
        let mut viewer = h.watch("someone@example.com").await;
        fake_adb::until("the hello", || {
            viewer.pump();
            viewer.hellos().len() == 1
        })
        .await;
        fake_adb::until("the panel goes dark", || {
            h.phone.servers()[0].control.iter().any(|m| m[0] == 10)
        })
        .await;
        let before = h.phone.servers()[0].control.len();

        let mut home = [0_u8; 14];
        home[5] = 3;
        let text = [1, 0, 0, 0, 2, b'h', b'i'];
        let back = [4, 0];
        viewer.control(&home);
        viewer.control(&text);
        viewer.control(&back);
        viewer.control(&[10, 1]);
        viewer.control(&[9, 0]);
        viewer.control(&[11]);
        fake_adb::until("the allowed input reaches the server", || {
            h.phone.servers()[0].control.len() >= before + 3
        })
        .await;
        time::sleep(Duration::from_millis(50)).await;
        let control = h.phone.servers()[0].control.clone();
        assert_eq!(
            control[before..].to_vec(),
            vec![home.to_vec(), text.to_vec(), back.to_vec()],
            "the relay keeps the panel dark itself; viewers can't light it"
        );

        viewer.detach();
        fake_adb::until("the audit row closes", || {
            h.audits().iter().all(|row| row.ends_with("closed"))
        })
        .await;
        assert_eq!(
            h.audits(),
            vec!["someone@example.com 2 1 3 closed".to_owned()]
        );
        h.screens.shutdown().await;
    });
}

#[test]
fn input_a_stalled_servers_full_write_queue_refuses_is_rejected_and_counted() {
    fake_adb::block_on(async {
        let h = Harness::start(screen_harness::test_timings()).await;
        let mut viewer = h.watch("someone").await;
        fake_adb::until("the hello", || {
            viewer.pump();
            viewer.hellos().len() == 1
        })
        .await;
        fake_adb::until("the panel goes dark", || {
            h.phone.log().contains(&"control:10".to_owned())
        })
        .await;
        h.phone.stall_control();
        let mut text = vec![1, 0, 0, 1, 44];
        text.extend([b'a'; 300]);
        let sent: usize = 100_000;
        for _ in 0..sent {
            viewer.control(&text);
        }
        viewer.detach();
        fake_adb::until("the audit row closes", || {
            h.audits().iter().all(|row| row.ends_with("closed"))
        })
        .await;
        let audit = h.audits().remove(0);
        let counts: Vec<usize> = audit
            .split(' ')
            .skip(1)
            .take(3)
            .map(|count| count.parse().unwrap())
            .collect();
        let [inputs, texts, rejected] = counts[..] else {
            panic!("an audit row has three counts: {audit}");
        };
        assert_eq!(inputs, 0, "{audit}");
        assert!(
            rejected > 0,
            "the write queue filled behind the stalled socket: {audit}"
        );
        assert!(
            texts >= 256,
            "the write queue took its backlog first: {audit}"
        );
        assert_eq!(texts + rejected, sent, "each text counted once: {audit}");
        h.screens.shutdown().await;
    });
}

#[test]
fn input_while_the_session_is_not_running_is_rejected_and_counted() {
    fake_adb::block_on(async {
        let h = Harness::start(screen_harness::test_timings()).await;
        h.phone.hold_starts();
        let viewer = h.watch("someone").await;
        fake_adb::until("the server runs", || h.phone.servers().len() == 1).await;
        viewer.control(&[4, 0]);
        viewer.detach();
        fake_adb::until("the audit row closes", || {
            h.audits().iter().all(|row| row.ends_with("closed"))
        })
        .await;
        assert_eq!(h.audits(), vec!["someone 0 0 1 closed".to_owned()]);
        h.phone.release_starts();
        h.screens.shutdown().await;
    });
}

#[test]
fn a_crashed_server_is_restarted_for_its_viewers_who_are_closed_once_retries_run_out() {
    fake_adb::block_on(async {
        let h = Harness::start(screen_harness::test_timings()).await;
        let mut viewer = h.watch("someone").await;
        fake_adb::until("the hello", || {
            viewer.pump();
            viewer.hellos().len() == 1
        })
        .await;

        h.phone.crash(0);
        fake_adb::until("a second hello", || {
            viewer.pump();
            viewer.hellos().len() == 2
        })
        .await;
        assert!(
            viewer
                .text
                .contains(&json!({"t": "status", "state": "restarting"}))
        );
        assert_eq!(h.phone.servers().len(), 2);

        h.phone.crash(1);
        fake_adb::until("a third hello", || {
            viewer.pump();
            viewer.hellos().len() == 3
        })
        .await;
        h.phone.crash(2);
        fake_adb::until("the viewer is closed", || {
            viewer.pump();
            viewer.closed.is_some()
        })
        .await;
        let closed = viewer.closed.clone().unwrap();
        assert_eq!(closed.code, 4002);
        assert!(
            closed.reason.starts_with("screen session failed: "),
            "{closed:?}"
        );
        fake_adb::until("the entry goes", || h.screens.list().is_empty()).await;
        h.screens.shutdown().await;
    });
}

#[test]
fn a_session_that_ran_long_enough_starts_its_restart_budget_over() {
    fake_adb::block_on(async {
        let h = Harness::start(ScreenTimings {
            restart_delays: vec![Duration::from_millis(10)],
            ..screen_harness::test_timings()
        })
        .await;
        let mut viewer = h.watch("someone").await;
        fake_adb::until("the hello", || {
            viewer.pump();
            viewer.hellos().len() == 1
        })
        .await;
        for crash in 0..3 {
            time::sleep(Duration::from_secs(10)).await;
            h.phone.crash(crash);
            fake_adb::until("the restart's hello", || {
                viewer.pump();
                viewer.hellos().len() == crash + 2
            })
            .await;
        }
        assert_eq!(viewer.closed, None, "each crash came after 10 s of running");
        h.phone.crash(3);
        fake_adb::until("the viewer is closed", || {
            viewer.pump();
            viewer.closed.is_some()
        })
        .await;
        assert_eq!(viewer.closed.map(|close| close.code), Some(4002));
        h.screens.shutdown().await;
    });
}

#[test]
fn unplugging_the_phone_closes_its_viewers_and_refuses_new_ones() {
    fake_adb::block_on(async {
        let h = Harness::start(screen_harness::test_timings()).await;
        let mut viewer = h.watch("someone").await;
        fake_adb::until("the hello", || {
            viewer.pump();
            viewer.hellos().len() == 1
        })
        .await;
        h.unplug();
        fake_adb::until("the viewer is closed", || {
            viewer.pump();
            viewer.closed.is_some()
        })
        .await;
        assert_eq!(viewer.closed, Some(ViewerClose::phone_offline()));
        fake_adb::until("the entry goes", || h.screens.list().is_empty()).await;
        let refused = h.screens.join(B30, "someone".to_owned()).await.unwrap_err();
        assert_eq!(refused, ViewerClose::not_online(B30));
        assert_eq!(h.audits(), vec!["someone 0 0 0 closed".to_owned()]);
        h.screens.shutdown().await;
    });
}

#[test]
fn a_shutting_down_relay_refuses_viewers_with_close_code_1001() {
    fake_adb::block_on(async {
        let h = Harness::start(screen_harness::test_timings()).await;
        h.screens.shutdown().await;
        let refused = h.screens.join(B30, "someone".to_owned()).await.unwrap_err();
        assert_eq!(refused.code, 1001);
        assert!(h.audits().is_empty(), "a refused viewer has no audit row");
    });
}

#[test]
fn every_viewer_gets_stats_every_interval_with_fps_to_one_decimal_and_whole_kbps() {
    fake_adb::block_on(async {
        let h = Harness::start(ScreenTimings {
            stats_interval: Duration::from_secs(2),
            ..screen_harness::test_timings()
        })
        .await;
        let mut viewer = h.watch("someone").await;
        fake_adb::until("stats", || {
            viewer.pump();
            viewer.text.iter().any(|message| message["t"] == "stats")
        })
        .await;
        let stats: Vec<_> = viewer
            .text
            .iter()
            .filter(|message| message["t"] == "stats")
            .collect();
        let fps = stats[0]["fps"].as_f64().unwrap();
        assert!(fps > 0.0, "{stats:?}");
        assert_eq!((fps * 10.0).fract(), 0.0, "one decimal: {fps}");
        assert!(stats[0]["kbps"].is_u64(), "whole kbps: {stats:?}");
        let listed = &h.screens.list()[0];
        let listed_fps = serde_json::to_value(listed.fps).unwrap().as_f64().unwrap();
        assert!((listed_fps - fps).abs() < 0.05, "{listed_fps} {fps}");
        h.screens.shutdown().await;
    });
}

#[test]
fn the_monitor_sees_a_session_as_active_from_its_start_until_it_stops() {
    fake_adb::block_on(async {
        let h = Harness::start(screen_harness::test_timings()).await;
        assert!(!h.screens.is_active(B30));
        h.phone.hold_starts();
        let viewer = h.watch("someone").await;
        fake_adb::until("the start", || h.screens.is_active(B30)).await;
        assert_eq!(h.screens.list()[0].state, ScreenState::Starting);
        h.phone.release_starts();
        fake_adb::until("the session runs", || {
            h.screens.list()[0].state == ScreenState::Running
        })
        .await;
        assert!(h.screens.is_active(B30));
        viewer.detach();
        fake_adb::until("the session stops", || !h.screens.is_active(B30)).await;
        h.screens.shutdown().await;
    });
}

#[test]
fn every_command_the_relay_sends_the_phone_is_one_of_the_known_ones() {
    fake_adb::block_on(async {
        let h = Harness::start(ScreenTimings {
            wake_check: Duration::from_millis(20),
            ..screen_harness::test_timings()
        })
        .await;
        let viewer = h.watch("someone").await;
        fake_adb::until("two wake checks", || h.phone.wakefulness_reads() >= 2).await;
        viewer.detach();
        fake_adb::until("the session stops", || h.screens.list().is_empty()).await;
        let servers = h.phone.servers();
        let scid = servers[0]
            .command
            .split(' ')
            .find_map(|argument| argument.strip_prefix("scid="))
            .unwrap()
            .to_owned();
        assert_eq!(
            servers[0].command,
            format!(
                "CLASSPATH=/data/local/tmp/scrcpy-server.jar app_process / \
                 com.genymobile.scrcpy.Server 3.3.3 scid={scid} log_level=info video=true \
                 audio=false control=true tunnel_forward=true max_size=0 max_fps=30 \
                 video_bit_rate=4000000 video_codec=h264 stay_awake=true \
                 clipboard_autosync=false send_device_meta=true send_dummy_byte=true 2>&1"
            )
        );
        let (head, last) = scid.split_at(7);
        assert_eq!(
            h.phone.kills(),
            vec![
                "pkill -f 'com.genymobile.scrcpy.Serve[r]' || true".to_owned(),
                format!("pkill -f 'scid={head}[{last}]' || true"),
            ]
        );
        h.screens.shutdown().await;
    });
}

/// Reads `viewer` until it has `count` binary frames.
async fn until_binary(viewer: &mut Watcher, count: usize, what: &str) {
    fake_adb::until(what, || {
        viewer.pump();
        viewer.binary.len() >= count
    })
    .await;
}

fn resets(h: &Harness) -> usize {
    h.phone.servers()[0]
        .control
        .iter()
        .filter(|message| message.as_slice() == RESET_VIDEO)
        .count()
}

#[test]
fn a_stalled_viewer_drops_frames_asks_for_one_keyframe_once_drained_and_leaves_others_alone() {
    screen_harness::block_on_real_time(async {
        let h = Harness::start(screen_harness::test_timings()).await;
        let mut fast = h.watch("fast").await;
        let mut slow = h.watch("slow").await;
        until_binary(&mut fast, 2, "the fast viewer's start").await;
        until_binary(&mut slow, 2, "the slow viewer's start").await;
        settle_resets().await;
        fast.pump();
        slow.pump();
        let fast_before = fast.binary.len();
        let slow_before = slow.binary.len();
        let resets_before = resets(&h);

        for frame in 1..=3 {
            h.phone.send_packet(0, 0, 300_000);
            until_binary(&mut fast, fast_before + frame, "the fast viewer's frame").await;
        }
        assert_eq!(
            resets(&h),
            resets_before,
            "nothing asked for while the queue is full"
        );

        slow.pump();
        assert_eq!(
            slow.binary.len(),
            slow_before + 2,
            "the third frame found more than a second of video queued and was dropped"
        );
        h.phone.send_packet(0, 0, 300_000);
        fake_adb::until("one keyframe request once drained", || {
            resets(&h) == resets_before + 1
        })
        .await;
        until_binary(
            &mut slow,
            slow_before + 4,
            "the config and keyframe it asked for",
        )
        .await;
        assert!(fake_scrcpy::is_config(&slow.binary[slow_before + 2]));
        assert!(fake_scrcpy::is_keyframe(&slow.binary[slow_before + 3]));
        h.phone.send_packet(0, 0, 1_000);
        until_binary(
            &mut slow,
            slow_before + 5,
            "frames again after the keyframe",
        )
        .await;
        until_binary(
            &mut fast,
            fast_before + 7,
            "every packet for the fast viewer",
        )
        .await;
        settle_resets().await;
        assert_eq!(resets(&h), resets_before + 1, "asked once");
        h.screens.shutdown().await;
    });
}

#[test]
fn every_session_pushes_the_jar_again() {
    fake_adb::block_on(async {
        let h = Harness::start(screen_harness::test_timings()).await;
        let mut viewer = h.watch("someone").await;
        fake_adb::until("the hello", || {
            viewer.pump();
            viewer.hellos().len() == 1
        })
        .await;
        assert_eq!(h.phone.pushes(), 1);
        h.phone.crash(0);
        fake_adb::until("the restart's hello", || {
            viewer.pump();
            viewer.hellos().len() == 2
        })
        .await;
        assert_eq!(
            h.phone.pushes(),
            2,
            "the server unlinks its jar, so a restart pushes it"
        );
        h.screens.shutdown().await;
    });
}

#[test]
fn stats_keep_coming_every_2_s() {
    fake_adb::block_on(async {
        let h = Harness::start(ScreenTimings {
            stats_interval: Duration::from_secs(2),
            ..screen_harness::test_timings()
        })
        .await;
        let mut viewer = h.watch("someone").await;
        fake_adb::until("the hello", || {
            viewer.pump();
            viewer.hellos().len() == 1
        })
        .await;
        let stats = |viewer: &Watcher| {
            viewer
                .text
                .iter()
                .filter(|message| message["t"] == "stats")
                .count()
        };
        let before = stats(&viewer);
        time::sleep(Duration::from_secs(10)).await;
        viewer.pump();
        assert_eq!(stats(&viewer) - before, 5, "one every 2 s");
        h.screens.shutdown().await;
    });
}

#[test]
fn a_server_that_never_sends_its_dummy_byte_fails_the_start_at_the_connect_timeout() {
    fake_adb::block_on(async {
        let h = Harness::start(ScreenTimings {
            restart_delays: Vec::new(),
            ..screen_harness::test_timings()
        })
        .await;
        h.phone.set_silent_video(true);
        let began = Instant::now();
        let mut viewer = h.watch("someone").await;
        fake_adb::until("the viewer is closed", || {
            viewer.pump();
            viewer.closed.is_some()
        })
        .await;
        let elapsed = began.elapsed();
        assert!(elapsed >= Duration::from_millis(500), "{elapsed:?}");
        assert!(elapsed < Duration::from_millis(700), "{elapsed:?}");
        assert_eq!(
            viewer.closed,
            Some(ViewerClose::session_failed(
                "scrcpy-server did not accept the video socket: the video socket gave no dummy \
                 byte in time"
            ))
        );
        h.screens.shutdown().await;
    });
}

#[test]
fn a_phone_unplugged_mid_start_closes_its_viewers_with_4001() {
    fake_adb::block_on(async {
        let h = Harness::start(screen_harness::test_timings()).await;
        h.phone.hold_starts();
        let mut viewer = h.watch("someone").await;
        fake_adb::until("the server runs", || h.phone.servers().len() == 1).await;
        h.unplug();
        fake_adb::until("the viewer is closed", || {
            viewer.pump();
            viewer.closed.is_some()
        })
        .await;
        assert_eq!(viewer.closed, Some(ViewerClose::phone_offline()));
        fake_adb::until("the entry goes", || h.screens.list().is_empty()).await;
        h.phone.release_starts();
        h.screens.shutdown().await;
    });
}

#[test]
fn a_restart_that_finds_the_phone_offline_closes_the_viewers_with_4001_not_4002() {
    fake_adb::block_on(async {
        let h = Harness::start(screen_harness::test_timings()).await;
        let mut viewer = h.watch("someone").await;
        fake_adb::until("the hello", || {
            viewer.pump();
            viewer.hellos().len() == 1
        })
        .await;
        h.go_offline_quietly();
        h.phone.crash(0);
        fake_adb::until("the viewer is closed", || {
            viewer.pump();
            viewer.closed.is_some()
        })
        .await;
        assert_eq!(viewer.closed, Some(ViewerClose::phone_offline()));
        assert_eq!(
            h.phone.servers().len(),
            1,
            "no session started for an offline phone"
        );
        h.screens.shutdown().await;
    });
}

#[test]
fn a_viewer_whose_audit_row_cant_be_written_is_refused_with_1011() {
    fake_adb::block_on(async {
        let h = Harness::start(screen_harness::test_timings()).await;
        h.break_audits();
        let refused = h.screens.join(B30, "someone".to_owned()).await.unwrap_err();
        assert_eq!(refused, ViewerClose::audit_failed());
        assert_eq!(refused.code, 1011);
        assert_eq!(h.events("screen-audit-failed").len(), 1);
        assert!(
            h.phone.servers().is_empty(),
            "nothing started for an unaudited viewer"
        );
        h.screens.shutdown().await;
    });
}

#[test]
fn the_idle_time_ending_during_a_held_start_cancels_the_start() {
    fake_adb::block_on(async {
        let h = Harness::start(ScreenTimings {
            idle: Duration::from_millis(50),
            ..screen_harness::test_timings()
        })
        .await;
        h.phone.hold_starts();
        let viewer = h.watch("someone").await;
        fake_adb::until("the server runs", || h.phone.servers().len() == 1).await;
        viewer.detach();
        fake_adb::until("the entry goes while the start is still held", || {
            h.screens.list().is_empty()
        })
        .await;
        assert!(
            !h.phone.servers()[0].alive,
            "the cancelled start's server was killed"
        );
        assert_eq!(h.scid_kills(), 1);
        h.phone.release_starts();
        time::sleep(Duration::from_millis(200)).await;
        assert_eq!(h.phone.servers().len(), 1, "nothing restarted");
        h.screens.shutdown().await;
    });
}

#[test]
fn a_dropped_viewer_handle_leaves_its_relay() {
    fake_adb::block_on(async {
        let h = Harness::start(screen_harness::test_timings()).await;
        let mut viewer = h.watch("someone").await;
        fake_adb::until("the hello", || {
            viewer.pump();
            viewer.hellos().len() == 1
        })
        .await;
        drop(viewer);
        fake_adb::until("the viewer has left", || {
            h.screens
                .list()
                .first()
                .is_none_or(|session| session.viewers == 0)
        })
        .await;
        fake_adb::until("the session stops after the idle time", || {
            h.screens.list().is_empty()
        })
        .await;
        h.screens.shutdown().await;
    });
}

#[test]
fn shutdown_waits_for_a_wake_in_flight() {
    fake_adb::block_on(async {
        let h = Harness::start(screen_harness::test_timings()).await;
        let mut viewer = h.watch("someone").await;
        fake_adb::until("the hello", || {
            viewer.pump();
            viewer.hellos().len() == 1
        })
        .await;
        fake_adb::until("the start's wake", || h.phone.wakes_done() == 1).await;
        h.phone.set_wake_delay(Duration::from_secs(3));
        let _joiner = h.watch("someone").await;
        fake_adb::until("the joiner's wake begins", || h.wakes() == 2).await;
        h.screens.shutdown().await;
        assert_eq!(
            h.phone.wakes_done(),
            2,
            "shutdown returned after the wake finished"
        );
    });
}

#[test]
fn the_list_keeps_phones_in_the_order_they_were_first_attached() {
    fake_adb::block_on(async {
        let h = Harness::start(screen_harness::test_timings()).await;
        screen_harness::online(&h.devices, "ZZZ0001");
        h.phone.hold_starts();
        let _z = h
            .screens
            .join("ZZZ0001", "someone".to_owned())
            .await
            .unwrap();
        let _b = h.watch("someone").await;
        fake_adb::until("both relays are listed", || h.screens.list().len() == 2).await;
        let serials: Vec<String> = h
            .screens
            .list()
            .into_iter()
            .map(|session| session.serial)
            .collect();
        assert_eq!(serials, vec!["ZZZ0001".to_owned(), B30.to_owned()]);
        h.screens.shutdown().await;
    });
}

#[test]
fn a_wall_clock_stepping_back_doesnt_hold_back_a_joiners_keyframe() {
    fake_adb::block_on(async {
        let stepped = Arc::new(AtomicBool::new(false));
        let tokio_clock = Clock::tokio(fake_adb::START);
        let step = Arc::clone(&stepped);
        let clock = Clock::from_fn(move || {
            let now = tokio_clock.now();
            if step.load(Ordering::Acquire) {
                now.minus(Duration::from_hours(1))
            } else {
                now
            }
        });
        let h = Harness::with_clock(screen_harness::test_timings(), clock).await;
        let mut first = h.watch("someone").await;
        until_binary(&mut first, 2, "the first viewer's packets").await;
        stepped.store(true, Ordering::Release);
        time::sleep(Duration::from_millis(100)).await;
        let mut second = h.watch("someone").await;
        until_binary(&mut second, 2, "the joiner's config and keyframe").await;
        assert!(fake_scrcpy::is_config(&second.binary[0]));
        h.screens.shutdown().await;
    });
}

#[test]
fn the_monitor_lists_the_phone_while_its_session_runs_and_unlists_it_once_its_server_is_killed() {
    fake_adb::block_on(async {
        let h = Harness::start(screen_harness::test_timings()).await;
        h.phone.set_kill_delay(KILL_DELAY);
        assert!(!h.monitor_sessions.is_active(B30));
        h.phone.hold_starts();
        let viewer = h.watch("someone").await;
        fake_adb::until("listed from the start", || {
            h.monitor_sessions.is_active(B30)
        })
        .await;
        h.phone.release_starts();
        fake_adb::until("the session runs", || {
            h.screens.list()[0].state == ScreenState::Running
        })
        .await;
        assert!(h.monitor_sessions.is_active(B30), "listed while running");
        viewer.detach();
        time::sleep(Duration::from_millis(50)).await;
        assert!(
            h.monitor_sessions.is_active(B30),
            "still listed during the idle time, while scrcpy runs"
        );
        listed_until_killed(&h).await;
        assert!(!h.phone.servers()[0].alive);
        h.screens.shutdown().await;
    });
}

#[test]
fn the_monitor_unlists_the_phone_once_a_failed_start_is_given_up_and_its_server_killed() {
    fake_adb::block_on(async {
        let h = Harness::start(ScreenTimings {
            restart_delays: Vec::new(),
            ..screen_harness::test_timings()
        })
        .await;
        h.phone.set_kill_delay(KILL_DELAY);
        h.phone.set_orphan(Orphan {
            unkillable: true,
            cleanup: Duration::ZERO,
        });
        let mut viewer = h.watch("someone").await;
        fake_adb::until("listed while starting", || {
            h.monitor_sessions.is_active(B30)
        })
        .await;
        fake_adb::until("the viewer is closed", || {
            viewer.pump();
            viewer.closed.is_some()
        })
        .await;
        assert_eq!(viewer.closed.map(|close| close.code), Some(4002));
        listed_until_killed(&h).await;
        h.screens.shutdown().await;
    });
}

#[test]
fn the_monitor_keeps_a_crashed_session_listed_until_its_server_is_killed() {
    fake_adb::block_on(async {
        let h = Harness::start(ScreenTimings {
            restart_delays: vec![Duration::from_secs(5)],
            ..screen_harness::test_timings()
        })
        .await;
        let mut viewer = h.watch("someone").await;
        fake_adb::until("the hello", || {
            viewer.pump();
            viewer.hellos().len() == 1
        })
        .await;
        h.phone.set_kill_delay(KILL_DELAY);
        h.phone.crash(0);
        listed_until_killed(&h).await;
        fake_adb::until("listed again for the restart", || {
            h.monitor_sessions.is_active(B30)
        })
        .await;
        h.screens.shutdown().await;
    });
}

#[test]
fn the_monitor_unlists_the_phone_at_shutdown_once_its_server_is_killed() {
    fake_adb::block_on(async {
        let h = Harness::start(screen_harness::test_timings()).await;
        let mut viewer = h.watch("someone").await;
        fake_adb::until("the hello", || {
            viewer.pump();
            viewer.hellos().len() == 1
        })
        .await;
        assert!(h.monitor_sessions.is_active(B30));
        h.phone.set_kill_delay(KILL_DELAY);
        let shutdown = pin::pin!(h.screens.shutdown());
        let unlisted = pin::pin!(listed_until_killed(&h));
        future::join(shutdown, unlisted).await;
        assert!(
            !h.monitor_sessions.is_active(B30),
            "unlisted by the time shutdown returns"
        );
    });
}
