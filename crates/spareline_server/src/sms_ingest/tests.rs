//! The phone manager runs against the stand-in adb server with the B30's recorded device lists
//! and probe output; ingest reads the B30's recorded provider output. New texts can't arrive
//! (the phone is out of service), so the ones added here are stand-ins written in the
//! provider's recorded format.

use std::fs;
use std::sync::atomic::AtomicI64;

use sha2::{Digest as _, Sha256};
use spareline_adb::{AdbClient, AdbError, AdbErrorKind, Endpoint};
use spareline_db::{
    AttachmentView, Database, IdentityReading, MessageScope, Paging, PhoneState, PhoneStateUpdate,
    SimReading, SlotContents, SlotReading,
};
use tokio::runtime::Builder;
use tokio::sync::broadcast::Receiver as BusReceiver;
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use super::*;
use crate::devices::{DeviceManager, DeviceOptions, Devices};
use crate::fake_adb::{self, Answer, B30, FakeAdb, NO_RESULT, PartRead, RECORDED_IDS, ScratchDir};

/// A JPEG's first bytes, the stand-in for an MMS photo.
const JPEG: [u8; 8] = [0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10, 0x4a, 0x46];

/// The parts of a stand-in MMS 7, in AOSP's `Mms.Part` layout: the slide layout, a text, and a
/// photo.
fn photo_parts() -> Vec<String> {
    vec![
        format!(
            "Row: 0 _id=29, mid=7, seq=-1, ct=application/smil, hex(name)={}, hex(fn)=, hex(cl)=, hex(text)={}",
            fake_adb::hex("smil.xml"),
            fake_adb::hex("<smil/>")
        ),
        format!(
            "Row: 1 _id=30, mid=7, seq=0, ct=text/plain, hex(name)=, hex(fn)=, hex(cl)={}, hex(text)={}",
            fake_adb::hex("text_0.txt"),
            fake_adb::hex("Here it is,\n\nfront view")
        ),
        format!(
            "Row: 2 _id=31, mid=7, seq=1, ct=image/jpeg, hex(name)={}, hex(fn)=, hex(cl)=, hex(text)=",
            fake_adb::hex("IMG 1, front.jpg")
        ),
    ]
}

fn png_part() -> Vec<String> {
    vec![
        "Row: 0 _id=40, mid=8, seq=0, ct=image/png, hex(name)=, hex(fn)=, hex(cl)=, hex(text)="
            .to_owned(),
    ]
}

/// A stand-in MMS row with its date in seconds.
struct StandInMms {
    id: i64,
    date: i64,
    sub_id: i64,
    parts: Vec<String>,
}

/// The phone manager and ingest on a stand-in B30, with an in-memory database, a clock the
/// test moves, and the texts ingest publishes.
struct Harness {
    adb: FakeAdb,
    db: DbHandle,
    manager: DeviceManager,
    ingest: SmsIngest,
    texts: BusReceiver<Message>,
    published: Vec<Message>,
    #[cfg_attr(
        dylint_lib = "trellis_lints",
        expect(
            shared_ownership,
            reason = "the clock every task reads is a closure over the time the test moves"
        )
    )]
    now: Arc<AtomicI64>,
    dir: ScratchDir,
}

impl Harness {
    /// A harness whose ticks the test drives: the trigger's own timer waits an hour.
    async fn new() -> Self {
        Self::with_trigger(Duration::from_hours(1)).await
    }

    async fn with_trigger(trigger_interval: Duration) -> Self {
        let adb = FakeAdb::sms().await;
        adb.answer(ANSWERS_COMMAND, "");
        let now = Arc::new(AtomicI64::new(fake_adb::START.millis()));
        let read = Arc::clone(&now);
        let clock = Clock::from_fn(move || Timestamp::from_millis(read.load(Ordering::Relaxed)));
        let database_clock = clock.clone();
        let db = DbHandle::new(
            Database::open_in_memory(move || database_clock.now())
                .expect("an in-memory database opens"),
        );
        let buses = Buses::new();
        let texts = buses.texts();
        let mut device_options = DeviceOptions::new(
            adb.client(),
            db.clone(),
            buses.clone(),
            "home".to_owned(),
            EventLog::discard(),
            clock.clone(),
            Duration::from_hours(24),
        );
        device_options.identity_retry = Vec::new();
        let manager = DeviceManager::new(device_options);
        let dir = ScratchDir::new();
        let mut options = SmsIngestOptions::new(
            manager.devices(),
            db.clone(),
            buses,
            AttachmentStore::new(dir.path().join("attachments")),
            EventLog::discard(),
            clock,
        );
        options.trigger_interval = trigger_interval;
        let mut ingest = SmsIngest::new(options);
        ingest.start().await.unwrap();
        Self {
            adb,
            db,
            manager,
            ingest,
            texts,
            published: Vec::new(),
            now,
            dir,
        }
    }

    /// Moves the test's clock on by `millis`.
    fn advance(&self, millis: i64) {
        self.now.fetch_add(millis, Ordering::Relaxed);
    }

    /// Brings the B30 online and waits for its probes and its first ingest cycle.
    async fn online(&mut self) {
        self.manager.start();
        self.adb.send_list(&fake_adb::recorded_track_lists()[0]);
        let devices = self.manager.devices();
        fake_adb::until("the first probes", || {
            devices
                .get(B30)
                .is_some_and(|phone| phone.health_at.is_some())
        })
        .await;
        self.manager.settled().await;
        fake_adb::until("ingest to watch the phone", || self.ingest.is_watching(B30)).await;
        self.ingest.settled().await;
    }

    async fn tick(&self, millis: i64) {
        self.advance(millis);
        self.ingest.tick(B30).await;
    }

    /// Every text published so far.
    fn published(&mut self) -> &[Message] {
        while let Ok(text) = self.texts.try_recv() {
            self.published.push(text);
        }
        &self.published
    }

    async fn all(&self) -> Vec<Message> {
        self.db
            .call(|db| db.messages().after(0, 1000))
            .await
            .unwrap()
    }

    async fn count(&self) -> usize {
        self.all().await.len()
    }

    async fn mms_count(&self) -> usize {
        let all = self.all().await;
        all.iter()
            .filter(|text| text.provider == spareline_db::Provider::Mms)
            .count()
    }

    async fn newest(&self) -> Message {
        self.all().await.pop().expect("a text is stored")
    }

    async fn get(&self, seq: i64) -> Option<Message> {
        self.db
            .call(move |db| db.messages().get(seq))
            .await
            .unwrap()
    }

    async fn cursor(&self, provider: Provider) -> i64 {
        let provider = stored_provider(provider);
        self.db
            .call(move |db| db.messages().ingest_cursor(1, provider))
            .await
            .unwrap()
    }

    /// The commands run on the B30 since `before` calls, without the serial.
    fn commands_since(&self, before: usize) -> Vec<String> {
        self.adb.calls()[before..]
            .iter()
            .map(|call| call[B30.len() + 2..].to_owned())
            .collect()
    }

    fn calls_ending_with(&self, command: &str) -> usize {
        self.adb
            .calls()
            .iter()
            .filter(|call| call.ends_with(command))
            .count()
    }

    fn answer_isms_records(&self, records: u32) {
        let isms = fake_adb::b30_fixture("isms.txt")
            .replace("total records=105", &format!("total records={records}"));
        self.adb.answer(spareline_core::ISMS_TRIGGER_COMMAND, &isms);
    }

    fn answer_mms(&self, mms: &StandInMms) {
        let StandInMms {
            id,
            date,
            sub_id,
            parts,
        } = mms;
        self.adb.answer(
            &ProviderQuery::Keys {
                provider: Provider::Mms,
            }
            .command(),
            &format!("Row: 0 _id={id}, date={date}\n"),
        );
        self.adb.answer(
            &ProviderQuery::ByIds {
                provider: Provider::Mms,
                ids: vec![*id],
            }
            .command(),
            &format!(
                "Row: 0 _id={id}, date={date}, date_sent={}, sub_id={sub_id}, m_type=132, hex(sub)={}\n",
                date - 5,
                fake_adb::hex("Photo")
            ),
        );
        let part_rows = if parts.is_empty() {
            NO_RESULT.to_owned()
        } else {
            format!("{}\n", parts.join("\n"))
        };
        self.adb.answer(
            &ProviderQuery::MmsParts { mms_ids: vec![*id] }.command(),
            &part_rows,
        );
        self.adb.answer(
            &ProviderQuery::MmsAddresses { mms_id: *id }.command(),
            "Row: 0 type=151, address=+15125550101\nRow: 1 type=137, address=+15125550155\n",
        );
    }

    async fn stop(self) {
        self.ingest.stop().await;
        self.manager.stop().await;
    }
}

fn iso(millis: i64) -> String {
    Timestamp::from_millis(millis).to_string()
}

fn after(provider: Provider, after_id: i64) -> String {
    ProviderQuery::After { provider, after_id }.command()
}

fn sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let digits: Vec<String> = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    digits.concat()
}

#[test]
fn a_phone_that_comes_online_has_its_inbox_backfilled_once_attributed_to_its_sims_number() {
    fake_adb::block_on(async {
        let mut harness = Harness::new().await;
        harness.online().await;
        let texts = harness.all().await;
        let ids: Vec<i64> = texts.iter().map(|text| text.provider_id).collect();
        assert_eq!(ids, RECORDED_IDS);
        for text in &texts {
            assert_eq!(
                text.number
                    .as_ref()
                    .and_then(|number| number.e164.as_deref()),
                Some("+15125550101"),
                "the SIM's recorded (fake) number, detected by the identity probe"
            );
        }
        let first = &texts[0];
        assert_eq!(first.provider, spareline_db::Provider::Sms);
        assert_eq!(first.phone, B30);
        assert_eq!(first.sub_id, Some(1));
        assert_eq!(first.from.as_deref(), Some("550"));
        assert_eq!(
            first.body,
            "Fake text 1, is the café table still for sale? ✓"
        );
        assert_eq!(first.sent_at, Some(iso(1_792_623_882_000)));
        assert_eq!(first.received_at, iso(1_792_637_380_314));
        assert_eq!(first.ingested_at, "2026-10-22T08:00:00.000Z");
        assert!(first.attachments.is_empty(), "an SMS has no parts");
        let published: Vec<i64> = harness.published().iter().map(|text| text.seq).collect();
        let stored: Vec<i64> = texts.iter().map(|text| text.seq).collect();
        assert_eq!(published, stored, "published once each, in seq order");
        assert_eq!(harness.cursor(Provider::Mms).await, 0);
        assert_eq!(harness.cursor(Provider::Sms).await, 11);

        assert_eq!(harness.ingest.reconcile(B30).await.unwrap(), 0);
        assert_eq!(harness.count().await, 8);
        assert_eq!(harness.published().len(), 8);
        harness.stop().await;
    });
}

#[test]
fn an_isms_change_pulls_only_the_rows_after_the_cursor_and_publishes_them() {
    fake_adb::block_on(async {
        let mut harness = Harness::new().await;
        harness.online().await;
        harness.tick(3000).await;
        assert!(
            harness
                .adb
                .calls()
                .last()
                .unwrap()
                .ends_with(spareline_core::ISMS_TRIGGER_COMMAND)
        );
        harness.answer_isms_records(111);
        harness.adb.answer(
            &after(Provider::Sms, 11),
            &format!(
                "Row: 0 _id=12, thread_id=4, address=+15125550153, date=1792656062003, date_sent=1792656060000, sub_id=1, read=0, type=1, hex(body)={}\n",
                fake_adb::hex("Hi, saw your ad")
            ),
        );
        let before = harness.adb.calls().len();
        harness.tick(3000).await;
        assert_eq!(
            harness.commands_since(before),
            [
                spareline_core::ISMS_TRIGGER_COMMAND.to_owned(),
                after(Provider::Sms, 11)
            ]
        );
        assert_eq!(harness.published().last().unwrap().seq, 9);
        let text = harness.get(9).await.unwrap();
        assert_eq!(text.provider_id, 12);
        assert_eq!(text.from.as_deref(), Some("+15125550153"));
        assert_eq!(text.body, "Hi, saw your ad");
        assert_eq!(text.number.unwrap().e164.as_deref(), Some("+15125550101"));
        assert_eq!(harness.cursor(Provider::Sms).await, 12);

        let quiet = harness.adb.calls().len();
        harness.tick(3000).await;
        assert_eq!(
            harness.adb.calls().len() - quiet,
            1,
            "an unchanged isms needs no provider query"
        );
        harness.stop().await;
    });
}

#[test]
fn an_isms_change_that_finds_nothing_yet_keeps_looking_for_three_ticks() {
    fake_adb::block_on(async {
        let mut harness = Harness::new().await;
        harness.online().await;
        harness.answer_isms_records(106);
        harness.adb.answer(&after(Provider::Mms, 0), NO_RESULT);
        for _ in 0..5 {
            harness.tick(3000).await;
        }
        assert_eq!(harness.calls_ending_with(&after(Provider::Sms, 11)), 3);
        assert_eq!(harness.calls_ending_with(&after(Provider::Mms, 0)), 3);
        harness.stop().await;
    });
}

#[test]
fn a_cleared_provider_whose_ids_start_over_is_reconciled_and_the_cursor_moves_back() {
    fake_adb::block_on(async {
        let mut harness = Harness::new().await;
        harness.online().await;
        harness.adb.answer(
            &ProviderQuery::Keys {
                provider: Provider::Sms,
            }
            .command(),
            "Row: 0 _id=1, date=1792700000000\n",
        );
        harness.adb.answer(
            &ProviderQuery::ByIds {
                provider: Provider::Sms,
                ids: vec![1],
            }
            .command(),
            &format!(
                "Row: 0 _id=1, thread_id=1, address=+15125550154, date=1792700000000, date_sent=1792699990000, sub_id=1, read=0, type=1, hex(body)={}\n",
                fake_adb::hex("After the reset")
            ),
        );
        harness.tick(61_000).await;
        assert_eq!(harness.count().await, 9);
        assert_eq!(harness.get(9).await.unwrap().body, "After the reset");
        assert_eq!(harness.cursor(Provider::Sms).await, 1);
        harness.stop().await;
    });
}

#[test]
fn an_mms_becomes_one_text_once_two_reads_agree_with_text_parts_as_the_body_and_files_for_the_rest()
{
    fake_adb::block_on(async {
        let mut harness = Harness::new().await;
        harness.online().await;
        harness.adb.set_part(
            31,
            PartRead::Output(Answer {
                stdout: JPEG.to_vec(),
                ..Answer::default()
            }),
        );
        harness.answer_mms(&StandInMms {
            id: 7,
            date: 1_792_656_100,
            sub_id: 1,
            parts: photo_parts(),
        });
        harness.tick(61_000).await;
        assert_eq!(harness.mms_count().await, 0, "one read isn't enough");
        harness.tick(3000).await;
        assert_eq!(harness.mms_count().await, 1);
        let mms = harness.newest().await;
        assert_eq!(mms.provider, spareline_db::Provider::Mms);
        assert_eq!(mms.provider_id, 7);
        assert_eq!(mms.from.as_deref(), Some("+15125550155"));
        assert_eq!(mms.subject.as_deref(), Some("Photo"));
        assert_eq!(mms.body, "Here it is,\n\nfront view");
        assert_eq!(mms.sent_at, Some(iso(1_792_656_095_000)));
        assert_eq!(mms.received_at, iso(1_792_656_100_000));
        assert_eq!(
            mms.number.as_ref().unwrap().e164.as_deref(),
            Some("+15125550101")
        );
        assert_eq!(
            mms.attachments,
            [AttachmentView {
                id: 1,
                content_type: "image/jpeg".to_owned(),
                size: 8,
                filename: Some("IMG 1, front.jpg".to_owned()),
                url: "/api/v1/attachments/1".to_owned(),
            }]
        );
        let sha = sha256(&JPEG);
        let file = harness
            .db
            .call(|db| db.messages().attachment(1))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(file.sha256, sha);
        assert_eq!(
            fs::read(harness.dir.path().join("attachments").join(&sha)).unwrap(),
            JPEG
        );
        assert_eq!(
            harness.calls_ending_with("content read --uri content://mms/part/31"),
            1
        );
        harness.tick(61_000).await;
        assert_eq!(harness.mms_count().await, 1, "a reconcile stores it once");
        harness.stop().await;
    });
}

#[test]
fn an_mms_read_mid_write_is_stored_once_with_its_final_date_and_parts() {
    fake_adb::block_on(async {
        let mut harness = Harness::new().await;
        harness.online().await;
        harness.adb.set_part(
            31,
            PartRead::Output(Answer {
                stdout: JPEG.to_vec(),
                ..Answer::default()
            }),
        );
        harness.answer_mms(&StandInMms {
            id: 7,
            date: 1_792_656_000,
            sub_id: -1,
            parts: Vec::new(),
        });
        harness.tick(61_000).await;
        harness.answer_mms(&StandInMms {
            id: 7,
            date: 1_792_656_100,
            sub_id: 1,
            parts: photo_parts(),
        });
        harness.tick(3000).await;
        assert_eq!(harness.mms_count().await, 0);
        harness.tick(3000).await;
        harness.tick(61_000).await;
        assert_eq!(harness.mms_count().await, 1);
        let mms = harness.newest().await;
        assert_eq!(mms.received_at, iso(1_792_656_100_000));
        assert_eq!(mms.sub_id, Some(1));
        assert_eq!(mms.attachments.len(), 1);
        assert_eq!(mms.attachments[0].content_type, "image/jpeg");
        assert_eq!(mms.attachments[0].size, 8);
        harness.stop().await;
    });
}

#[test]
fn a_part_read_that_reports_an_error_is_retried_and_never_stored_as_the_attachment() {
    fake_adb::block_on(async {
        let mut harness = Harness::new().await;
        harness.online().await;
        harness.adb.set_part(31, PartRead::Output(Answer {
            stdout: b"partial".to_vec(),
            stderr: b"Error while accessing provider:mms\njava.io.FileNotFoundException: No entry for content://mms/part/31\n".to_vec(),
            exit_code: 0,
        }));
        harness.answer_mms(&StandInMms {
            id: 7,
            date: 1_792_656_100,
            sub_id: 1,
            parts: photo_parts(),
        });
        harness.tick(61_000).await;
        harness.tick(3000).await;
        assert_eq!(harness.mms_count().await, 0);
        harness.adb.set_part(31, PartRead::NoExitPacket);
        harness.tick(61_000).await;
        assert_eq!(
            harness.mms_count().await,
            0,
            "a shell without its exit status is no better"
        );
        harness.adb.set_part(
            31,
            PartRead::Output(Answer {
                stdout: JPEG.to_vec(),
                ..Answer::default()
            }),
        );
        harness.tick(61_000).await;
        assert_eq!(harness.mms_count().await, 1);
        let stored = harness.newest().await;
        let sizes: Vec<i64> = stored.attachments.iter().map(|part| part.size).collect();
        assert_eq!(sizes, [8]);
        let id = stored.attachments[0].id;
        let file = harness
            .db
            .call(move |db| db.messages().attachment(id))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(file.sha256, sha256(&JPEG));
        harness.stop().await;
    });
}

#[test]
fn an_unreadable_part_gives_up_after_five_reconciles_not_counting_follow_up_reads() {
    fake_adb::block_on(async {
        let mut harness = Harness::new().await;
        harness.online().await;
        harness.answer_mms(&StandInMms {
            id: 8,
            date: 1_792_656_200,
            sub_id: 1,
            parts: png_part(),
        });
        harness.tick(61_000).await;
        harness.tick(3000).await;
        for attempt in 1..=4 {
            harness.tick(61_000).await;
            harness.tick(3000).await;
            assert_eq!(harness.mms_count().await, 0, "attempt {attempt}");
        }
        harness.tick(61_000).await;
        assert_eq!(harness.mms_count().await, 1);
        let mms = harness.newest().await;
        assert_eq!(mms.provider, spareline_db::Provider::Mms);
        assert!(mms.attachments.is_empty(), "stored without the part");
        harness.stop().await;
    });
}

#[test]
fn an_mms_row_without_an_integer_id_is_skipped_and_the_rest_still_ingest() {
    fake_adb::block_on(async {
        let mut harness = Harness::new().await;
        harness.online().await;
        harness.adb.set_part(
            31,
            PartRead::Output(Answer {
                stdout: JPEG.to_vec(),
                ..Answer::default()
            }),
        );
        harness.answer_mms(&StandInMms {
            id: 7,
            date: 1_792_656_100,
            sub_id: 1,
            parts: photo_parts(),
        });
        harness.adb.answer(
            &ProviderQuery::Keys {
                provider: Provider::Mms,
            }
            .command(),
            "Row: 0 _id=7, date=1792656100\nRow: 1 _id=x, date=1792656101\n",
        );
        harness.adb.answer(
            &ProviderQuery::ByIds {
                provider: Provider::Mms,
                ids: vec![7],
            }
            .command(),
            "Row: 0 _id=7, date=1792656100, date_sent=1792656095, sub_id=1, m_type=132, hex(sub)=\nRow: 1 _id=x, date=1792656101, date_sent=0, sub_id=1, m_type=132, hex(sub)=\n",
        );
        harness.tick(61_000).await;
        harness.tick(3000).await;
        assert_eq!(harness.mms_count().await, 1);
        assert!(
            !harness
                .adb
                .calls()
                .iter()
                .any(|call| call.contains("mid IN (7,")),
            "the bad row's id is never queried"
        );
        harness.stop().await;
    });
}

#[test]
fn ingest_waits_for_the_phones_identity_so_the_first_texts_already_have_their_number() {
    fake_adb::block_on(async {
        let mut harness = Harness::new().await;
        harness.adb.hold();
        harness.manager.start();
        harness.adb.send_list(&fake_adb::recorded_track_lists()[0]);
        let devices = harness.manager.devices();
        fake_adb::until("online", || {
            devices
                .get(B30)
                .is_some_and(|phone| phone.status == PhoneStatus::Online)
        })
        .await;
        time::sleep(Duration::from_millis(50)).await;
        assert!(
            !harness
                .adb
                .calls()
                .iter()
                .any(|call| call.contains("content query")),
            "no provider query before the identity is read"
        );
        harness.adb.release();
        fake_adb::until("the backfill", || harness.published().len() == 8).await;
        harness.manager.settled().await;
        harness.ingest.settled().await;
        let numbers: Vec<Option<String>> = harness
            .published()
            .iter()
            .map(|text| text.number.as_ref().and_then(|number| number.e164.clone()))
            .collect();
        assert_eq!(numbers, vec![Some("+15125550101".to_owned()); 8]);
        harness.stop().await;
    });
}

#[test]
fn texts_stored_without_a_number_are_published_again_at_new_seqs_once_the_sim_is_known() {
    fake_adb::block_on(async {
        let mut harness = Harness::new().await;
        harness.online().await;
        harness.advance(3000);
        harness.answer_isms_records(112);
        harness.adb.answer(
            &after(Provider::Sms, 11),
            &format!(
                "Row: 0 _id=12, thread_id=5, address=+15125550158, date=1792656062003, date_sent=1792656060000, sub_id=2, read=0, type=1, hex(body)={}\n",
                fake_adb::hex("On the second SIM")
            ),
        );
        harness.ingest.tick(B30).await;
        assert_eq!(harness.get(9).await.unwrap().number, None);
        let second_sim = IdentityReading {
            slots: vec![SlotReading {
                slot: 1,
                contents: SlotContents::Present(Some(SimReading {
                    sub_id: 2,
                    iccid: Some("8901555000000000029".to_owned()),
                    carrier: None,
                    number: Some("+15125550102".to_owned()),
                })),
            }],
            ..IdentityReading::default()
        };
        let placed_at = Timestamp::parse_iso("2026-10-22T08:00:10.000Z").unwrap();
        harness
            .db
            .call(move |db| db.inventory().apply_identity(1, &second_sim, placed_at))
            .await
            .unwrap();
        let before = harness.published().len();
        harness.ingest.attribute_pending().await.unwrap();
        let republished: Vec<i64> = harness.published()[before..]
            .iter()
            .map(|text| text.seq)
            .collect();
        assert_eq!(republished, [10]);
        assert_eq!(
            harness
                .published()
                .last()
                .unwrap()
                .number
                .as_ref()
                .unwrap()
                .e164
                .as_deref(),
            Some("+15125550102")
        );
        assert_eq!(harness.get(9).await, None, "the old seq stays a gap");
        let page = harness
            .db
            .call(|db| {
                db.messages().page(
                    &MessageScope {
                        number_id: Some(2),
                        search: None,
                    },
                    Paging::After {
                        after: 8,
                        limit: 10,
                    },
                )
            })
            .await
            .unwrap();
        let moved: Vec<(i64, i64)> = page
            .messages
            .iter()
            .map(|text| (text.seq, text.provider_id))
            .collect();
        assert_eq!(moved, [(10, 12)]);
        harness.stop().await;
    });
}

#[test]
fn a_change_of_sims_on_a_phone_gives_pending_texts_their_number() {
    fake_adb::block_on(async {
        let mut harness = Harness::new().await;
        harness.online().await;
        harness.advance(3000);
        harness.answer_isms_records(112);
        harness.adb.answer(
            &after(Provider::Sms, 11),
            &format!(
                "Row: 0 _id=12, thread_id=5, address=+15125550158, date=1792656062003, date_sent=1792656060000, sub_id=2, read=0, type=1, hex(body)={}\n",
                fake_adb::hex("On the second SIM")
            ),
        );
        harness.ingest.tick(B30).await;
        let second_sim = IdentityReading {
            slots: vec![SlotReading {
                slot: 1,
                contents: SlotContents::Present(Some(SimReading {
                    sub_id: 2,
                    iccid: Some("8901555000000000029".to_owned()),
                    carrier: None,
                    number: Some("+15125550102".to_owned()),
                })),
            }],
            ..IdentityReading::default()
        };
        let placed_at = Timestamp::parse_iso("2026-10-22T08:00:10.000Z").unwrap();
        harness
            .db
            .call(move |db| db.inventory().apply_identity(1, &second_sim, placed_at))
            .await
            .unwrap();
        harness.manager.devices().emit(PhoneEvent::SubsChanged {
            serial: B30.to_owned(),
        });
        fake_adb::until("the text at its new seq", || {
            harness
                .published()
                .last()
                .is_some_and(|text| text.seq == 10)
        })
        .await;
        harness.stop().await;
    });
}

#[test]
fn a_phone_that_goes_away_stops_being_polled() {
    fake_adb::block_on(async {
        let mut harness = Harness::new().await;
        harness.online().await;
        harness.adb.send_list("");
        fake_adb::until("ingest to stop watching", || {
            !harness.ingest.is_watching(B30)
        })
        .await;
        let before = harness.adb.calls().len();
        harness.ingest.tick(B30).await;
        assert_eq!(harness.adb.calls().len(), before);
        harness.stop().await;
    });
}

#[test]
fn a_failed_reconcile_is_retried_after_15_s_not_on_every_tick() {
    fake_adb::block_on(async {
        let mut harness = Harness::new().await;
        harness.online().await;
        let keys = ProviderQuery::Keys {
            provider: Provider::Sms,
        }
        .command();
        let before = harness.calls_ending_with(&keys);
        harness
            .adb
            .set_failing(Some("adb transport closed (stand-in)"));
        harness.tick(61_000).await;
        harness.adb.set_failing(None);
        harness.tick(3000).await;
        assert_eq!(harness.calls_ending_with(&keys), before);
        harness.tick(11_000).await;
        assert_eq!(
            harness.calls_ending_with(&keys),
            before,
            "14 s after the failure"
        );
        harness.tick(2000).await;
        assert_eq!(
            harness.calls_ending_with(&keys),
            before + 1,
            "16 s after the failure"
        );
        harness.stop().await;
    });
}

#[test]
fn an_mms_with_an_unreadable_part_is_retried_on_reconciles_only_not_read_again_every_tick() {
    fake_adb::block_on(async {
        let mut harness = Harness::new().await;
        harness.online().await;
        harness.answer_mms(&StandInMms {
            id: 8,
            date: 1_792_656_200,
            sub_id: 1,
            parts: png_part(),
        });
        let before = harness.adb.calls().len();
        let reads = |harness: &Harness| {
            harness
                .commands_since(before)
                .iter()
                .filter(|command| command.contains("content read"))
                .count()
        };
        let mut first_read = None;
        for tick in 1..=120 {
            harness.tick(3000).await;
            if first_read.is_none() && reads(&harness) > 0 {
                first_read = Some(tick);
            }
        }
        let queries = harness
            .commands_since(before)
            .iter()
            .filter(|command| command.contains("content query"))
            .count();
        assert_eq!(
            first_read,
            Some(21),
            "the reconcile at 60 s reads the MMS once, and the trigger's read at 63 s agrees and \
             reads the part"
        );
        assert_eq!(
            reads(&harness),
            6,
            "one read when two reads agree, then one per reconcile until the fifth gives up"
        );
        assert!(queries <= 40, "{queries}");
        assert_eq!(harness.mms_count().await, 1);
        harness.stop().await;
    });
}

#[test]
fn a_stored_mms_is_not_read_again_by_the_triggers_follow_up_queries() {
    fake_adb::block_on(async {
        let mut harness = Harness::new().await;
        harness.online().await;
        harness.adb.set_part(
            31,
            PartRead::Output(Answer {
                stdout: JPEG.to_vec(),
                ..Answer::default()
            }),
        );
        harness.answer_mms(&StandInMms {
            id: 7,
            date: 1_792_656_100,
            sub_id: 1,
            parts: photo_parts(),
        });
        harness.adb.answer(
            &after(Provider::Mms, 0),
            &format!(
                "Row: 0 _id=7, date=1792656100, date_sent=1792656095, sub_id=1, m_type=132, hex(sub)={}\n",
                fake_adb::hex("Photo")
            ),
        );
        harness.adb.answer(&after(Provider::Mms, 7), NO_RESULT);
        harness.answer_isms_records(106);
        let before = harness.adb.calls().len();
        for _ in 0..4 {
            harness.tick(3000).await;
        }
        assert_eq!(harness.mms_count().await, 1);
        let commands = harness.commands_since(before);
        assert_eq!(
            commands
                .iter()
                .filter(|command| command.contains("content read"))
                .count(),
            1
        );
        assert_eq!(
            commands
                .iter()
                .filter(|command| command.contains("/addr"))
                .count(),
            1
        );
        assert_eq!(harness.cursor(Provider::Mms).await, 7);
        harness.stop().await;
    });
}

#[test]
fn a_malformed_row_is_logged_by_its_id_only() {
    assert_eq!(
        row_id_prefix("Row: 3 _id=17, thread_id=2, address=+15125550101"),
        Some("Row: 3 _id=17")
    );
    assert_eq!(row_id_prefix("Row: 3 thread_id=2"), None);
    assert_eq!(row_id_prefix("garbage"), None);
}

#[test]
fn the_trigger_runs_by_itself_every_three_seconds() {
    fake_adb::block_on(async {
        let mut harness = Harness::with_trigger(TRIGGER_INTERVAL).await;
        harness.online().await;
        let isms = spareline_core::ISMS_TRIGGER_COMMAND;
        let before = harness.calls_ending_with(isms);
        harness.answer_isms_records(111);
        harness.adb.answer(
            &after(Provider::Sms, 11),
            &format!(
                "Row: 0 _id=12, thread_id=4, address=+15125550153, date=1792656062003, date_sent=1792656060000, sub_id=1, read=0, type=1, hex(body)={}\n",
                fake_adb::hex("Hi, saw your ad")
            ),
        );
        time::advance(Duration::from_millis(2900)).await;
        assert_eq!(harness.calls_ending_with(isms), before, "not before 3 s");
        fake_adb::until("the text", || {
            harness.published().last().is_some_and(|text| text.seq == 9)
        })
        .await;
        assert_eq!(harness.calls_ending_with(isms), before + 1);
        harness.stop().await;
    });
}

#[test]
fn concurrent_loops_publish_texts_in_the_order_they_committed() {
    Builder::new_multi_thread()
        .worker_threads(8)
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let db = DbHandle::new(Database::open_in_memory(Timestamp::now).unwrap());
            let update = PhoneStateUpdate {
                serial: B30.to_owned(),
                state: PhoneState::Device,
                site: "home".to_owned(),
                at: fake_adb::START,
                raw_state: None,
            };
            let phone_id = db
                .call(move |db| db.inventory().record_phone_state(&update))
                .await
                .unwrap()
                .phone_id;
            let buses = Buses::new();
            let mut texts = buses.texts();
            let dir = ScratchDir::new();
            let ingest = SmsIngest::new(SmsIngestOptions::new(
                Devices::new(
                    AdbClient::new(Endpoint::new("127.0.0.1".to_owned(), 9)),
                    Clock::system(),
                ),
                db,
                buses,
                AttachmentStore::new(dir.path().join("attachments")),
                EventLog::discard(),
                Clock::system(),
            ));
            let mut writers = JoinSet::new();
            for provider_id in 1..=400 {
                let ingest = Arc::clone(&ingest.ingest);
                writers.spawn(async move {
                    let text = NewMessage {
                        provider: spareline_db::Provider::Sms,
                        provider_id,
                        sub_id: Some(1),
                        sender: Some("550".to_owned()),
                        body: "Stand-in".to_owned(),
                        subject: None,
                        sent_at: None,
                        received_at: fake_adb::START,
                        attachments: Vec::new(),
                    };
                    ingest.store(B30, phone_id, vec![text]).await.unwrap();
                });
            }
            while let Some(written) = writers.join_next().await {
                written.unwrap();
            }
            let mut seqs = Vec::new();
            while let Ok(text) = texts.try_recv() {
                seqs.push(text.seq);
            }
            let mut sorted = seqs.clone();
            sorted.sort_unstable();
            assert_eq!(seqs.len(), 400);
            assert_eq!(
                seqs, sorted,
                "a stream would drop every seq published after a higher one"
            );
        });
}

#[test]
fn reconcile_and_confirm_read_a_backlog_of_mms_in_batches_of_100() {
    fake_adb::block_on(async {
        let mut harness = Harness::new().await;
        harness.online().await;
        let ids: Vec<i64> = (1001..=1150).collect();
        let date = |id: i64| 1_792_656_000 + id;
        let keys: Vec<String> = ids
            .iter()
            .enumerate()
            .map(|(row, id)| format!("Row: {row} _id={id}, date={}", date(*id)))
            .collect();
        harness.adb.answer(
            &ProviderQuery::Keys {
                provider: Provider::Mms,
            }
            .command(),
            &format!("{}\n", keys.join("\n")),
        );
        for batch in ids.chunks(100) {
            let rows: Vec<String> = batch
                .iter()
                .enumerate()
                .map(|(row, id)| {
                    format!(
                        "Row: {row} _id={id}, date={}, date_sent=0, sub_id=1, m_type=132, hex(sub)=",
                        date(*id)
                    )
                })
                .collect();
            harness.adb.answer(
                &ProviderQuery::ByIds {
                    provider: Provider::Mms,
                    ids: batch.to_vec(),
                }
                .command(),
                &format!("{}\n", rows.join("\n")),
            );
            harness.adb.answer(
                &ProviderQuery::MmsParts {
                    mms_ids: batch.to_vec(),
                }
                .command(),
                NO_RESULT,
            );
        }
        for id in &ids {
            harness.adb.answer(
                &ProviderQuery::MmsAddresses { mms_id: *id }.command(),
                "Row: 0 type=137, address=+15125550155\n",
            );
        }
        harness.tick(61_000).await;
        assert_eq!(
            harness.mms_count().await,
            0,
            "every MMS waits for a second read"
        );
        harness.tick(3000).await;
        assert_eq!(harness.mms_count().await, 150);
        let by_ids: Vec<usize> = harness
            .adb
            .calls()
            .iter()
            .filter_map(|call| {
                let list = call.split("m_type=132 AND _id IN (").nth(1)?;
                Some(list.split(')').next()?.split(',').count())
            })
            .collect();
        assert_eq!(
            by_ids,
            [100, 50, 100, 50],
            "the reconcile's two batches, then the confirmation's"
        );
        harness.stop().await;
    });
}

#[test]
fn a_part_read_that_times_out_breaks_the_protocol_or_loses_its_exit_code_gives_up_after_five() {
    for read in [
        PartRead::Hang,
        PartRead::ExitWithoutCode,
        PartRead::NoExitPacket,
    ] {
        fake_adb::block_on(async {
            let mut harness = Harness::new().await;
            harness.online().await;
            harness.adb.set_part(40, read.clone());
            harness.answer_mms(&StandInMms {
                id: 8,
                date: 1_792_656_200,
                sub_id: 1,
                parts: png_part(),
            });
            harness.tick(61_000).await;
            harness.tick(3000).await;
            for reconcile in 1..=4 {
                harness.tick(61_000).await;
                harness.tick(3000).await;
                assert_eq!(
                    harness.mms_count().await,
                    0,
                    "{read:?}, reconcile {reconcile}"
                );
                assert_eq!(
                    harness
                        .ingest
                        .ingest
                        .part_failures()
                        .values()
                        .copied()
                        .collect::<Vec<u32>>(),
                    [reconcile],
                    "{read:?} counts on each reconcile"
                );
            }
            harness.tick(61_000).await;
            assert_eq!(harness.mms_count().await, 1, "{read:?}");
            assert!(
                harness.newest().await.attachments.is_empty(),
                "{read:?}: stored without the part"
            );
            harness.stop().await;
        });
    }
}

#[test]
fn a_part_read_the_adb_server_refuses_counts_nothing() {
    fake_adb::block_on(async {
        let mut harness = Harness::new().await;
        harness.online().await;
        harness
            .adb
            .set_part(40, PartRead::Refused("device offline (stand-in)"));
        harness.answer_mms(&StandInMms {
            id: 8,
            date: 1_792_656_200,
            sub_id: 1,
            parts: png_part(),
        });
        harness.tick(61_000).await;
        harness.tick(3000).await;
        for reconcile in 1..=6 {
            harness.tick(61_000).await;
            harness.tick(3000).await;
            assert_eq!(harness.mms_count().await, 0, "reconcile {reconcile}");
        }
        assert!(
            harness.ingest.ingest.part_failures().is_empty(),
            "a refusal says nothing about the part"
        );
        harness.adb.set_part(
            40,
            PartRead::Output(Answer {
                stdout: JPEG.to_vec(),
                ..Answer::default()
            }),
        );
        harness.tick(61_000).await;
        assert_eq!(harness.mms_count().await, 1);
        assert_eq!(
            harness.newest().await.attachments.len(),
            1,
            "stored with its part"
        );
        harness.stop().await;
    });
}

#[test]
fn a_part_read_that_loses_its_exit_code_as_the_phone_goes_counts_nothing() {
    fake_adb::block_on(async {
        let mut harness = Harness::new().await;
        harness.online().await;
        harness.adb.set_part(40, PartRead::NoExitPacket);
        harness
            .adb
            .refuse(ANSWERS_COMMAND, "device offline (stand-in)");
        harness.answer_mms(&StandInMms {
            id: 8,
            date: 1_792_656_200,
            sub_id: 1,
            parts: png_part(),
        });
        harness.tick(61_000).await;
        harness.tick(3000).await;
        for reconcile in 1..=6 {
            harness.tick(61_000).await;
            harness.tick(3000).await;
            assert_eq!(harness.mms_count().await, 0, "reconcile {reconcile}");
        }
        assert!(
            harness.ingest.ingest.part_failures().is_empty(),
            "a phone that stopped answering says nothing about the part"
        );
        assert!(
            harness.calls_ending_with(ANSWERS_COMMAND) >= 6,
            "each lost exit code was checked against the phone"
        );
        harness.adb.answer(ANSWERS_COMMAND, "");
        harness.tick(61_000).await;
        assert_eq!(
            harness
                .ingest
                .ingest
                .part_failures()
                .values()
                .copied()
                .collect::<Vec<u32>>(),
            [1],
            "with the phone answering, the same lost exit code counts"
        );
        harness.stop().await;
    });
}

#[test]
fn a_part_read_cut_short_by_a_disconnect_counts_nothing() {
    fake_adb::block_on(async {
        let mut harness = Harness::new().await;
        harness.online().await;
        harness.adb.set_part(40, PartRead::Hang);
        harness.answer_mms(&StandInMms {
            id: 8,
            date: 1_792_656_200,
            sub_id: 1,
            parts: png_part(),
        });
        harness.tick(61_000).await;
        harness.tick(3000).await;
        harness.advance(61_000);
        {
            let before = harness.adb.calls().len();
            let reconcile = pin::pin!(harness.ingest.tick(B30));
            let unplug = pin::pin!(async {
                fake_adb::until("the reconcile's part read", || {
                    harness.adb.calls()[before..]
                        .iter()
                        .any(|call| call.contains("content read"))
                })
                .await;
                harness.adb.send_list("");
            });
            future::join(reconcile, unplug).await;
        }
        assert_eq!(
            harness
                .ingest
                .ingest
                .part_failures()
                .values()
                .copied()
                .collect::<Vec<u32>>(),
            [0],
            "only the trigger's read was seen, and a phone that went away says nothing about \
             the part"
        );
        harness.stop().await;
    });
}

#[test]
fn a_failed_part_call_counts_only_when_it_timed_out_broke_the_protocol_or_lost_its_exit_code() {
    let devices = Devices::new(
        AdbClient::new(Endpoint::new("127.0.0.1".to_owned(), 9)),
        Clock::system(),
    );
    let token = {
        let now = devices.clock().now();
        let mut table = devices.table();
        table.apply_state(B30, PhoneState::Device, Some("1-1"), now);
        table
            .start_session(B30, mpsc::unbounded_channel().0)
            .unwrap()
            .token
    };
    let timeout = || {
        DeviceError::Adb(AdbError::new(
            AdbErrorKind::Timeout,
            "the adb operation timed out".to_owned(),
        ))
    };
    assert_eq!(
        PartFailure::of_call(timeout(), &token),
        PartFailure::Unreadable("the adb operation timed out".to_owned())
    );
    for (kind, counts) in [
        (AdbErrorKind::Protocol, true),
        (AdbErrorKind::Closed, true),
        (AdbErrorKind::Connect, false),
        (AdbErrorKind::Fail, false),
    ] {
        let error = DeviceError::Adb(AdbError::new(kind, "stand-in".to_owned()));
        let expected = if counts {
            PartFailure::Unreadable("stand-in".to_owned())
        } else {
            PartFailure::Device(error.clone())
        };
        assert_eq!(PartFailure::of_call(error, &token), expected, "{kind:?}");
    }
    let gone = DeviceError::Disconnected {
        serial: B30.to_owned(),
    };
    assert_eq!(
        PartFailure::of_call(gone.clone(), &token),
        PartFailure::Device(gone)
    );
    let offline = DeviceError::NotOnline {
        serial: B30.to_owned(),
        state: "offline".to_owned(),
    };
    assert_eq!(
        PartFailure::of_call(offline.clone(), &token),
        PartFailure::Device(offline)
    );
    devices.table().end_session(B30);
    assert_eq!(
        PartFailure::of_call(timeout(), &token),
        PartFailure::Device(timeout()),
        "a call that failed as its session ended failed because the phone went away"
    );
}

#[test]
fn stopping_cancels_a_cycle_in_flight() {
    fake_adb::block_on(async {
        let mut harness = Harness::with_trigger(TRIGGER_INTERVAL).await;
        harness.online().await;
        let before = harness.adb.calls().len();
        harness.adb.hold();
        fake_adb::until("a cycle to reach the phone", || {
            harness.adb.calls().len() > before
        })
        .await;
        let Harness {
            adb,
            manager,
            ingest,
            ..
        } = harness;
        time::timeout(Duration::from_secs(1), ingest.stop())
            .await
            .expect("stop doesn't wait for a phone that doesn't answer");
        adb.release();
        manager.stop().await;
    });
}

#[test]
fn stopping_waits_for_a_loop_unwatched_just_before() {
    fake_adb::block_on(async {
        let mut harness = Harness::new().await;
        harness.online().await;
        harness.adb.send_list("");
        fake_adb::until("ingest to stop watching", || {
            !harness.ingest.is_watching(B30)
        })
        .await;
        assert_eq!(
            harness.ingest.ingest.retired().len(),
            1,
            "kept until its task ends"
        );
        let Harness {
            manager, ingest, ..
        } = harness;
        let shared = Arc::clone(&ingest.ingest);
        ingest.stop().await;
        assert!(shared.retired().is_empty(), "stop took it");
        assert!(shared.loops().is_empty());
        assert_eq!(
            Arc::strong_count(&shared),
            1,
            "the retired loop's task, which holds the ingest state, ended before stop returned"
        );
        manager.stop().await;
    });
}

#[test]
fn part_failures_are_forgotten_when_the_mms_leaves_the_inbox() {
    fake_adb::block_on(async {
        let mut harness = Harness::new().await;
        harness.online().await;
        let failing = StandInMms {
            id: 8,
            date: 1_792_656_200,
            sub_id: 1,
            parts: png_part(),
        };
        harness.answer_mms(&failing);
        harness.tick(61_000).await;
        harness.tick(3000).await;
        harness.tick(61_000).await;
        let failures = || harness.ingest.ingest.part_failures().clone();
        assert_eq!(
            failures().values().copied().collect::<Vec<u32>>(),
            [1],
            "one failed reconcile"
        );
        harness.adb.answer(
            &ProviderQuery::Keys {
                provider: Provider::Mms,
            }
            .command(),
            NO_RESULT,
        );
        harness.tick(61_000).await;
        assert!(failures().is_empty(), "the MMS left the inbox");
        harness.stop().await;
    });
}

#[test]
fn part_failures_are_kept_while_the_phone_is_away_so_a_flaky_phone_still_gives_up() {
    fake_adb::block_on(async {
        let mut harness = Harness::new().await;
        harness.online().await;
        harness.answer_mms(&StandInMms {
            id: 8,
            date: 1_792_656_200,
            sub_id: 1,
            parts: png_part(),
        });
        harness.tick(61_000).await;
        harness.tick(3000).await;
        harness.tick(61_000).await;
        harness.tick(61_000).await;
        let failures = |harness: &Harness| -> Vec<u32> {
            harness
                .ingest
                .ingest
                .part_failures()
                .values()
                .copied()
                .collect()
        };
        assert_eq!(failures(&harness), [2], "two failed reconciles");
        harness.adb.send_list("");
        fake_adb::until("ingest to stop watching", || {
            !harness.ingest.is_watching(B30)
        })
        .await;
        assert_eq!(failures(&harness), [2], "kept while the phone is away");
        harness.adb.send_list(&fake_adb::recorded_track_lists()[0]);
        fake_adb::until("ingest to watch the phone again", || {
            harness.ingest.is_watching(B30)
        })
        .await;
        harness.ingest.settled().await;
        harness.tick(3000).await;
        for reconcile in 3..=4 {
            harness.tick(61_000).await;
            assert_eq!(failures(&harness), [reconcile], "counting goes on");
        }
        assert_eq!(harness.mms_count().await, 0);
        harness.tick(61_000).await;
        assert_eq!(
            harness.mms_count().await,
            1,
            "the fifth failed reconcile, over two connections, gives up on the part"
        );
        assert!(
            harness.newest().await.attachments.is_empty(),
            "stored without the part"
        );
        harness.stop().await;
    });
}

#[test]
fn a_follower_that_falls_behind_watches_the_ready_phones_it_missed() {
    fake_adb::block_on(async {
        let harness = Harness::new().await;
        let devices = harness.manager.devices();
        {
            let now = devices.clock().now();
            let mut table = devices.table();
            table.apply_state(B30, PhoneState::Device, Some("1-1"), now);
            let session = table
                .start_session(B30, mpsc::unbounded_channel().0)
                .unwrap()
                .token
                .id();
            let identity = spareline_core::parse_identity(&fake_adb::b30_fixture("identity.txt"))
                .into_identity();
            table.set_identity(B30, session, identity, now);
        }
        for index in 0..300 {
            devices.emit(PhoneEvent::Connected {
                serial: format!("OTHER{index}"),
            });
        }
        fake_adb::until("the missed phone to be watched", || {
            harness.ingest.is_watching(B30)
        })
        .await;
        harness.stop().await;
    });
}
