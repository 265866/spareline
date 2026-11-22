-- The initial schema. Times are ISO 8601 UTC strings.

-- Inventory: phones, SIMs, numbers, and where each SIM is. Device facts are
-- written by the devices module; the typed fields and number notes are
-- written by people through the API.

CREATE TABLE phones (
	id INTEGER PRIMARY KEY,
	serial TEXT NOT NULL UNIQUE,
	site TEXT NOT NULL,
	manufacturer TEXT,
	model TEXT,
	android_version TEXT,
	sdk INTEGER,
	build_fingerprint TEXT,
	imei1 TEXT,
	imei2 TEXT,
	location TEXT,
	hub_port TEXT,
	cost_cents INTEGER CHECK (cost_cents IS NULL OR cost_cents >= 0),
	acquired_on TEXT,
	-- The last adb state seen (device, unauthorized, offline, gone, ...).
	last_state TEXT NOT NULL,
	first_seen_at TEXT NOT NULL,
	-- The last time the phone was online (adb state "device").
	last_online_at TEXT
);

CREATE TABLE sims (
	id INTEGER PRIMARY KEY,
	iccid TEXT NOT NULL UNIQUE,
	carrier TEXT,
	account_ref TEXT,
	plan TEXT,
	renews_on TEXT,
	cost_cents INTEGER CHECK (cost_cents IS NULL OR cost_cents >= 0),
	status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'spare', 'retired')),
	first_seen_at TEXT NOT NULL,
	last_seen_at TEXT NOT NULL
);

-- Which phone and slot a SIM was in, and when. Open placements have no to_at.
CREATE TABLE sim_placements (
	id INTEGER PRIMARY KEY,
	sim_id INTEGER NOT NULL REFERENCES sims (id),
	phone_id INTEGER NOT NULL REFERENCES phones (id),
	slot INTEGER NOT NULL CHECK (slot >= 0),
	sub_id INTEGER NOT NULL,
	from_at TEXT NOT NULL,
	to_at TEXT,
	CHECK (to_at IS NULL OR to_at >= from_at)
);
CREATE UNIQUE INDEX sim_placements_open_sim ON sim_placements (sim_id) WHERE to_at IS NULL;
CREATE UNIQUE INDEX sim_placements_open_slot ON sim_placements (phone_id, slot) WHERE to_at IS NULL;

-- A phone number. e164 is null until it is detected from the SIM or typed by
-- a person; e164_source says which, and a manual value is never overwritten
-- by detection.
CREATE TABLE numbers (
	id INTEGER PRIMARY KEY,
	e164 TEXT UNIQUE CHECK (e164 IS NULL OR e164 GLOB '+[1-9]*'),
	e164_source TEXT NOT NULL CHECK (e164_source IN ('detected', 'manual', 'none')),
	note TEXT NOT NULL DEFAULT '',
	created_at TEXT NOT NULL,
	CHECK ((e164 IS NULL) = (e164_source = 'none'))
);

-- Which SIM carried a number, and when. Internal plumbing: it keeps old
-- messages on the right number when a SIM moves.
CREATE TABLE number_bindings (
	id INTEGER PRIMARY KEY,
	number_id INTEGER NOT NULL REFERENCES numbers (id),
	sim_id INTEGER NOT NULL REFERENCES sims (id),
	from_at TEXT NOT NULL,
	to_at TEXT,
	-- What the SIM reported, normalized, at the last probe.
	detected_e164 TEXT,
	-- Who typed the number, when a person set it.
	confirmed_by TEXT,
	CHECK (to_at IS NULL OR to_at >= from_at)
);
CREATE UNIQUE INDEX number_bindings_open_sim ON number_bindings (sim_id) WHERE to_at IS NULL;
CREATE UNIQUE INDEX number_bindings_open_number ON number_bindings (number_id) WHERE to_at IS NULL;

-- What happened to phones and SIMs, for the UI and the audit trail. phone_id
-- is null for events about the adb server itself.
CREATE TABLE device_events (
	id INTEGER PRIMARY KEY,
	phone_id INTEGER REFERENCES phones (id),
	at TEXT NOT NULL,
	kind TEXT NOT NULL,
	detail_json TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX device_events_phone ON device_events (phone_id, id);

-- Edits people make to inventory, with their login when one is known.
CREATE TABLE inventory_edits (
	id INTEGER PRIMARY KEY,
	at TEXT NOT NULL,
	login TEXT,
	entity TEXT NOT NULL,
	entity_id INTEGER NOT NULL,
	changes_json TEXT NOT NULL
);

-- Inbound texts, read from each phone's SMS and MMS providers (src/sms/).
-- seq is the global cursor for every API that pages, waits, or streams; one
-- writer commits in seq order, so `after=<seq>` never skips a row. A text
-- stored before its number was known moves to a new, higher seq when the
-- number is found, so cursors for that number see it.
CREATE TABLE messages (
	seq INTEGER PRIMARY KEY AUTOINCREMENT,
	phone_id INTEGER NOT NULL REFERENCES phones (id),
	provider TEXT NOT NULL CHECK (provider IN ('sms', 'mms')),
	-- The provider row's _id. It can repeat if Messages' data is cleared, so
	-- the dedup key includes received_at.
	provider_id INTEGER NOT NULL,
	sub_id INTEGER,
	-- Resolved from (phone, sub_id) through sim_placements, then through
	-- number_bindings; null until the SIM is known, and filled in later.
	sim_id INTEGER REFERENCES sims (id),
	number_id INTEGER REFERENCES numbers (id),
	sender TEXT,
	body TEXT NOT NULL DEFAULT '',
	-- MMS only.
	subject TEXT,
	-- Carrier time (date_sent), the phone's clock (date), and the server's clock.
	sent_at TEXT,
	received_at TEXT NOT NULL,
	ingested_at TEXT NOT NULL,
	UNIQUE (phone_id, provider, provider_id, received_at)
);
CREATE INDEX messages_number ON messages (number_id, seq);
CREATE INDEX messages_unattributed ON messages (seq) WHERE number_id IS NULL;

-- MMS parts other than text and SMIL. The bytes are a file named by sha256
-- under data/attachments/.
CREATE TABLE attachments (
	id INTEGER PRIMARY KEY,
	message_seq INTEGER NOT NULL REFERENCES messages (seq),
	content_type TEXT NOT NULL,
	size INTEGER NOT NULL CHECK (size >= 0),
	sha256 TEXT NOT NULL CHECK (length(sha256) = 64),
	filename TEXT
);
CREATE INDEX attachments_message ON attachments (message_seq);

-- The highest provider _id ingested per phone and provider.
CREATE TABLE ingest_cursors (
	phone_id INTEGER NOT NULL REFERENCES phones (id),
	provider TEXT NOT NULL CHECK (provider IN ('sms', 'mms')),
	last_provider_id INTEGER NOT NULL,
	PRIMARY KEY (phone_id, provider)
);

-- One row per focus viewer of a phone's screen (the view that can send
-- input). Inputs and typed texts are counted; text content is never stored.
-- `rejected` counts control messages the relay refused. ended_at stays null
-- if the server stopped without closing the viewer.
CREATE TABLE control_sessions (
	id INTEGER PRIMARY KEY,
	phone_id INTEGER NOT NULL REFERENCES phones (id),
	login TEXT,
	started_at TEXT NOT NULL,
	ended_at TEXT,
	inputs INTEGER NOT NULL DEFAULT 0,
	texts INTEGER NOT NULL DEFAULT 0,
	rejected INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX control_sessions_phone ON control_sessions (phone_id, started_at);

-- Five-minute health aggregates (src/monitor/), one row per phone per
-- bucket, written from the in-memory samples so the SD card sees few writes.
CREATE TABLE health_5m (
	phone_id INTEGER NOT NULL REFERENCES phones (id),
	-- Start of the bucket, ISO 8601 UTC.
	bucket TEXT NOT NULL,
	samples INTEGER NOT NULL CHECK (samples > 0),
	-- Mean battery level and highest temperature (tenths of a degree C).
	battery_pct INTEGER,
	battery_temp_dc INTEGER,
	-- By slot: {"0": {"minLevel": 2, "dbm": -110}}.
	signal_json TEXT NOT NULL DEFAULT '{}',
	-- By slot: share of samples in service, {"0": 1}.
	service_json TEXT NOT NULL DEFAULT '{}',
	-- Internet mobile bytes over the trailing 24 h at the last data watch.
	mobile_rx INTEGER,
	mobile_tx INTEGER,
	PRIMARY KEY (phone_id, bucket)
) WITHOUT ROWID;
