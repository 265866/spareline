import type { Database } from "bun:sqlite";
import { afterEach, beforeEach, expect, test } from "bun:test";
import { FakeAdb, recordedTrackLists } from "../../test/fake-adb";
import { Bus } from "../bus";
import { openDb } from "../db/db";
import { Inventory } from "../inventory/store";
import type { DeviceEventRow } from "../inventory/sync";
import { silent } from "../log";
import { DeviceManager } from "./manager";

// A real phone cannot be unplugged, replugged, or first-plugged on demand in a
// test run, so these tests feed DeviceManager the device lists the adb server
// really sent during a transport reconnect, and answer its probes with output
// recorded from the B30.

const B30 = "SPARELINE00001";
let db: Database;
let adb: FakeAdb;
let manager: DeviceManager;
let events: DeviceEventRow[];

beforeEach(async () => {
	db = openDb(":memory:");
	adb = await FakeAdb.b30();
	events = [];
	const bus = new Bus<DeviceEventRow>();
	bus.subscribe((event) => events.push(event));
	manager = new DeviceManager({
		adb,
		db,
		site: "home",
		events: bus,
		log: silent,
		healthIntervalMs: 60_000,
		identityRetryMs: [1, 1],
		maxBackoffMs: 5,
	});
});
afterEach(() => manager.stop());

async function until(check: () => boolean, what: string): Promise<void> {
	for (let i = 0; i < 200; i++) {
		if (check()) return;
		await Bun.sleep(5);
	}
	throw new Error(`timed out waiting for ${what}`);
}

test("a phone coming online is probed and recorded", async () => {
	const [first] = await recordedTrackLists();
	manager.start();
	adb.send(first ?? []);
	await until(
		() =>
			manager.get(B30)?.health !== null &&
			manager.get(B30)?.health !== undefined,
		"health",
	);
	await manager.settled();
	const live = manager.get(B30);
	expect(live?.status).toBe("online");
	expect(live?.identity?.model).toBe("S6702X");
	expect(live?.health?.battery?.level).toBe(85);
	expect(manager.adb()).toMatchObject({ reachable: true, version: 41 });
	expect(events.map((event) => event.kind)).toEqual([
		"phone.added",
		"phone.online",
		"sim.added",
		"sim.placed",
		"number.bound",
	]);
	expect(events[0]?.serial).toBe(B30);
	const phone = new Inventory(db).getPhone(B30);
	expect(phone?.placements[0]?.number?.e164).toBe("+15125550101");
});

test("a recorded transport reconnect yields one offline and one online event", async () => {
	const lists = await recordedTrackLists();
	expect(
		lists.map((list) => list.map((entry) => entry.state).join() || "none"),
	).toEqual([
		"device",
		"offline",
		"none",
		"offline",
		"authorizing",
		"offline",
		"device",
	]);
	manager.start();
	adb.send(lists[0] ?? []);
	await until(
		() => events.some((event) => event.kind === "number.bound"),
		"first probe",
	);
	await manager.settled();
	const seen: string[] = [];
	manager.on("disconnected", (serial) => seen.push(`disconnected ${serial}`));
	manager.on("connected", (serial) => seen.push(`connected ${serial}`));
	events.length = 0;
	for (const list of lists.slice(1)) adb.send(list);
	await until(
		() => events.some((event) => event.kind === "phone.online"),
		"back online",
	);
	await manager.settled();
	expect(events.map((event) => event.kind)).toEqual([
		"phone.offline",
		"phone.online",
	]);
	expect(seen).toEqual([`disconnected ${B30}`, `connected ${B30}`]);
	// Reconnecting re-probes the phone, which finds nothing new.
	expect(
		adb.calls.filter((call) => call.includes("dumpsys isub")),
	).toHaveLength(2);
});

test("an unauthorized phone is recorded and shown as needing auth, without probes", async () => {
	manager.start();
	adb.send([
		{
			serial: "NEWPHONE",
			state: "unauthorized",
			rawState: "unauthorized",
			usb: "1-2",
			transportId: 9,
		},
	]);
	await until(() => manager.get("NEWPHONE") !== undefined, "new phone");
	expect(manager.get("NEWPHONE")?.status).toBe("needs_auth");
	expect(events.map((event) => event.kind)).toEqual([
		"phone.added",
		"phone.needs_auth",
	]);
	expect(adb.calls).toEqual([]);
	expect(() => manager.exec("NEWPHONE", "id")).toThrow("not online");
});

test("losing the adb server marks phones offline and reconnects", async () => {
	const [first] = await recordedTrackLists();
	manager.start();
	adb.send(first ?? []);
	await until(() => manager.get(B30)?.status === "online", "online");
	await manager.settled();
	events.length = 0;
	adb.reachable = false;
	adb.send("end");
	await until(() => manager.adb().reachable === false, "adb down");
	expect(manager.get(B30)?.status).toBe("offline");
	expect(events.map((event) => event.kind)).toEqual([
		"adb.unreachable",
		"phone.offline",
	]);
	const calls = adb.versionCalls;
	await until(() => adb.versionCalls > calls + 1, "retries");
	adb.reachable = true;
	adb.send(first ?? []);
	await until(() => manager.get(B30)?.status === "online", "online again");
	await manager.settled();
	expect(events.map((event) => event.kind)).toEqual([
		"adb.unreachable",
		"phone.offline",
		"adb.reachable",
		"phone.online",
	]);
});

test("a phone the database last saw online is marked offline when this run does not see it", async () => {
	db.run(
		"INSERT INTO phones (serial, site, last_state, first_seen_at, last_online_at) VALUES ('OLD', 'home', 'device', '2026-10-20T00:00:00Z', '2026-10-20T00:00:00Z')",
	);
	manager.start();
	adb.send([]);
	await until(() => manager.get("OLD") !== undefined, "stale phone");
	expect(manager.get("OLD")?.status).toBe("offline");
	expect(new Inventory(db).getPhone("OLD")?.lastState).toBe("gone");
	expect(events.map((event) => [event.kind, event.serial])).toEqual([
		["phone.offline", "OLD"],
	]);
});

test("a SIM state change in the health round re-reads identity", async () => {
	const [first] = await recordedTrackLists();
	manager.start();
	adb.send(first ?? []);
	await until(
		() => events.some((event) => event.kind === "number.bound"),
		"probe",
	);
	await manager.settled();
	const health = (
		await Bun.file("test/fixtures/device/nuu-s6702x/health.txt").text()
	).replace("LOADED,ABSENT", "LOADED,LOADED");
	const { HEALTH_SCRIPT } = await import("./probe");
	adb.answer(HEALTH_SCRIPT, health);
	const isubCalls = () =>
		adb.calls.filter((call) => call.includes("dumpsys isub")).length;
	const before = isubCalls();
	manager.tick();
	await manager.settled();
	expect(isubCalls()).toBe(before + 1);
});

test("exec and push go to online phones through their session", async () => {
	const [first] = await recordedTrackLists();
	manager.start();
	adb.send(first ?? []);
	await until(() => manager.get(B30)?.status === "online", "online");
	expect(new TextDecoder().decode(await manager.exec(B30, "true"))).toBe(
		"exec-ok",
	);
	await manager.push(B30, new Uint8Array(10), "/data/local/tmp/x");
	expect(adb.pushes).toEqual([
		{ serial: B30, remote: "/data/local/tmp/x", size: 10 },
	]);
	await manager.settled();
});

test("a phone the database last saw online is offline when adb is unreachable at startup", async () => {
	db.run(
		"INSERT INTO phones (serial, site, last_state, first_seen_at, last_online_at) VALUES ('OLD', 'home', 'device', '2026-10-20T00:00:00Z', '2026-10-20T00:00:00Z')",
	);
	adb.reachable = false;
	manager.start();
	await until(() => manager.get("OLD") !== undefined, "stale phone");
	expect(manager.adb().reachable).toBe(false);
	expect(manager.get("OLD")?.status).toBe("offline");
	expect(new Inventory(db).getPhone("OLD")?.lastState).toBe("gone");
});

test("a phone that comes back while its last probe is unwinding is probed again", async () => {
	const lists = await recordedTrackLists();
	const device = lists[0] ?? [];
	let release = () => {};
	adb.hold = new Promise((resolve) => {
		release = resolve;
	});
	manager.start();
	adb.send(device);
	await until(() => manager.get(B30)?.status === "online", "online");
	// Unplugged and replugged while the first identity probe is still stuck.
	adb.send([]);
	await until(() => manager.get(B30)?.status === "offline", "offline");
	adb.send(device);
	await until(() => manager.get(B30)?.status === "online", "online again");
	adb.hold = null;
	release();
	await manager.settled();
	expect(manager.get(B30)?.identity?.model).toBe("S6702X");
	expect(manager.get(B30)?.health?.battery?.level).toBe(85);
	expect(
		adb.calls.filter((call) => call.includes("dumpsys isub")),
	).toHaveLength(2);
});
