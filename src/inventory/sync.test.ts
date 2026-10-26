import type { Database } from "bun:sqlite";
import { beforeEach, expect, test } from "bun:test";
import { fixture, ReplayShell } from "../../test/replay-shell";
import { openDb } from "../db/db";
import {
	IDENTITY_SCRIPT,
	type PhoneIdentity,
	probeIdentity,
} from "../devices/probe";
import { Inventory } from "./store";
import { applyIdentity, recordPhoneState } from "./sync";

const B30 = "SPARELINE00001";
const T0 = "2026-10-21T10:00:00.000Z";
const T1 = "2026-10-21T11:00:00.000Z";
const T2 = "2026-10-21T12:00:00.000Z";

let db: Database;
let inventory: Inventory;
let identity: PhoneIdentity;

beforeEach(async () => {
	db = openDb(":memory:");
	inventory = new Inventory(db);
	const shell = new ReplayShell()
		.answer(IDENTITY_SCRIPT, await fixture("nuu-s6702x", "identity.txt"))
		.answerPrefix(
			"printf '\\n@@sl:imei:0\\n'",
			await fixture("nuu-s6702x", "subscriber.txt"),
		);
	identity = await probeIdentity(shell, B30);
});

const online = (serial: string, at: string) =>
	recordPhoneState(db, { serial, state: "device", site: "home", at }).phoneId;
const kinds = (events: { kind: string }[]) => events.map((event) => event.kind);
const clone = (value: PhoneIdentity): PhoneIdentity => structuredClone(value);

test("phone states: first sight, needs auth, online, transient connecting, offline", () => {
	const first = recordPhoneState(db, {
		serial: B30,
		state: "unauthorized",
		site: "home",
		at: T0,
	});
	expect(kinds(first.events)).toEqual(["phone.added", "phone.needs_auth"]);
	expect(
		kinds(
			recordPhoneState(db, {
				serial: B30,
				state: "device",
				site: "home",
				at: T1,
			}).events,
		),
	).toEqual(["phone.online"]);
	expect(
		kinds(
			recordPhoneState(db, {
				serial: B30,
				state: "offline",
				site: "home",
				at: T2,
			}).events,
		),
	).toEqual(["phone.offline"]);
	expect(
		kinds(
			recordPhoneState(db, {
				serial: B30,
				state: "authorizing",
				site: "home",
				at: T2,
			}).events,
		),
	).toEqual([]);
	expect(
		kinds(
			recordPhoneState(db, { serial: B30, state: "gone", site: "home", at: T2 })
				.events,
		),
	).toEqual([]);
	expect(
		kinds(
			recordPhoneState(db, {
				serial: B30,
				state: "no_permissions",
				site: "home",
				at: T2,
			}).events,
		),
	).toEqual(["phone.no_permissions"]);
	const phone = inventory.getPhone(B30);
	expect(phone?.lastState).toBe("no_permissions");
	expect(phone?.firstSeenAt).toBe(T0);
	expect(phone?.lastOnlineAt).toBe(T1);
});

test("first identity records the phone, its SIM, placement, and detected number", () => {
	const phoneId = online(B30, T0);
	expect(kinds(applyIdentity(db, phoneId, identity, T0))).toEqual([
		"sim.added",
		"sim.placed",
		"number.bound",
	]);
	const phone = inventory.getPhone(B30);
	expect(phone).toMatchObject({
		model: "S6702X",
		androidVersion: "14",
		imeis: ["350000000000006", "350000000000014"],
	});
	expect(phone?.placements).toEqual([
		{
			slot: 0,
			subId: 1,
			since: T0,
			sim: {
				id: 1,
				iccid: "8901555000000000001",
				carrier: "T-Mobile",
			},
			number: {
				id: 1,
				e164: "+15125550101",
				source: "detected",
				note: "",
			},
		},
	]);
	// Probing again changes nothing.
	expect(applyIdentity(db, phoneId, identity, T1)).toEqual([]);
	expect(inventory.getPhone(B30)?.placements[0]?.since).toBe(T0);
});

test("a removed SIM closes its placement; the number remembers where it was", () => {
	const phoneId = online(B30, T0);
	applyIdentity(db, phoneId, identity, T0);
	const removed = clone(identity);
	removed.slots = [
		{ slot: 0, state: "empty", simState: "ABSENT" },
		{ slot: 1, state: "empty", simState: "ABSENT" },
	];
	removed.sims = [];
	expect(kinds(applyIdentity(db, phoneId, removed, T1))).toEqual([
		"sim.removed",
	]);
	expect(inventory.getPhone(B30)?.placements).toEqual([]);
	expect(inventory.findNumber("+15125550101")?.phone).toEqual({
		serial: B30,
		model: "S6702X",
		slot: 0,
		removedAt: T1,
	});
	expect(inventory.getSim(1)?.lastPlacement).toEqual({
		serial: B30,
		slot: 0,
		until: T1,
	});
});

test("a slot that is still loading keeps its placement", () => {
	const phoneId = online(B30, T0);
	applyIdentity(db, phoneId, identity, T0);
	const loading = clone(identity);
	loading.slots[0] = { slot: 0, state: "unknown", simState: "NOT_READY" };
	loading.sims = [];
	expect(applyIdentity(db, phoneId, loading, T1)).toEqual([]);
	expect(inventory.getPhone(B30)?.placements).toHaveLength(1);
});

test("a SIM moved to another phone takes its number along", () => {
	const a = online(B30, T0);
	applyIdentity(db, a, identity, T0);
	const b = online("OTHERPHONE", T1);
	const moved = clone(identity);
	moved.slots = [
		{ slot: 0, state: "empty", simState: "ABSENT" },
		{ slot: 1, state: "present", simState: "LOADED" },
	];
	moved.sims = moved.sims.map((sim) => ({ ...sim, slot: 1, subId: 2 }));
	const events = applyIdentity(db, b, moved, T1);
	expect(events.map((event) => [event.kind, event.phoneId])).toEqual([
		["sim.removed", a],
		["sim.placed", b],
	]);
	expect(inventory.getPhone(B30)?.placements).toEqual([]);
	const number = inventory.findNumber("+15125550101");
	expect(number?.phone).toMatchObject({
		serial: "OTHERPHONE",
		slot: 1,
		removedAt: null,
	});
	expect(number?.id).toBe(1);
});

test("a number typed by the owner survives detection and can be cleared", () => {
	const phoneId = online(B30, T0);
	applyIdentity(db, phoneId, identity, T0);
	inventory.patchNumber(1, { e164: "+15125550199" }, "owner@example.com");
	expect(inventory.getNumber(1)).toMatchObject({
		e164: "+15125550199",
		source: "manual",
		detectedE164: "+15125550101",
		confirmedBy: "owner@example.com",
	});
	expect(applyIdentity(db, phoneId, identity, T1)).toEqual([]);
	expect(inventory.getNumber(1)?.e164).toBe("+15125550199");
	inventory.patchNumber(1, { e164: null }, null);
	expect(inventory.getNumber(1)).toMatchObject({
		e164: "+15125550101",
		source: "detected",
		confirmedBy: null,
	});
	const audit = db
		.query("SELECT login, entity, entity_id FROM inventory_edits ORDER BY id")
		.all();
	expect(audit).toEqual([
		{ login: "owner@example.com", entity: "number", entity_id: 1 },
		{ login: null, entity: "number", entity_id: 1 },
	]);
});

test("a SIM without a detected number gets an empty number the owner can fill in", () => {
	const phoneId = online(B30, T0);
	const silent = clone(identity);
	for (const sim of silent.sims) sim.number = null;
	applyIdentity(db, phoneId, silent, T0);
	expect(inventory.getNumber(1)).toMatchObject({ e164: null, source: "none" });
	// Detection later fills it in.
	expect(kinds(applyIdentity(db, phoneId, identity, T1))).toEqual([
		"number.detected",
	]);
	expect(inventory.getNumber(1)).toMatchObject({
		e164: "+15125550101",
		source: "detected",
	});
});

test("a SIM that starts reporting a different number moves to a new number row", () => {
	const phoneId = online(B30, T0);
	applyIdentity(db, phoneId, identity, T0);
	const changed = clone(identity);
	for (const sim of changed.sims) sim.number = "+15125550102";
	expect(kinds(applyIdentity(db, phoneId, changed, T1))).toEqual([
		"number.changed",
	]);
	expect(inventory.findNumber("+15125550102")?.phone?.serial).toBe(B30);
	expect(inventory.findNumber("+15125550101")?.sim?.iccid).toBe(
		"8901555000000000001",
	);
	expect(inventory.getPhone(B30)?.placements[0]?.number?.e164).toBe(
		"+15125550102",
	);
});

test("owner edits: a number keeps its note across probes, conflicts are refused", () => {
	const phoneId = online(B30, T0);
	applyIdentity(db, phoneId, identity, T0);
	expect(
		inventory.patchNumber(1, { note: "Craigslist couch" }, null).note,
	).toBe("Craigslist couch");
	applyIdentity(db, phoneId, identity, T1);
	expect(inventory.getPhone(B30)?.placements[0]?.number?.note).toBe(
		"Craigslist couch",
	);
	online("SECOND", T0);
	const second = clone(identity);
	for (const sim of second.sims) {
		sim.iccid = "8901555000000000002";
		sim.number = null;
	}
	applyIdentity(db, inventory.getPhone("SECOND")?.id ?? 0, second, T0);
	expect(() =>
		inventory.patchNumber(2, { e164: "+15125550101" }, null),
	).toThrow("already belongs");
});
