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
