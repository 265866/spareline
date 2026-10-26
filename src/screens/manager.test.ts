import type { Database } from "bun:sqlite";
import { afterEach, beforeEach, expect, test } from "bun:test";
import { FakePhone, type FakeServer, FakeSocket } from "../../test/fake-scrcpy";
import { openDb } from "../db/db";
import { CloseCode, ScreenManager } from "./manager";
import { RESET_VIDEO, setDisplayPower } from "./protocol";

const SERIAL = "SPARELINE00001";
let phone: FakePhone;
let db: Database;
let screens: ScreenManager;
let logged: { event: string; fields: Record<string, unknown> }[];
const events = (name: string) => logged.filter((entry) => entry.event === name);
const WAKE = "exec:input keyevent KEYCODE_WAKEUP; wm dismiss-keyguard";
const wakes = () => phone.log.filter((entry) => entry === WAKE).length;

function manager(options: {
	idleMs?: number;
	restartDelaysMs?: number[];
	wakeCheckMs?: number;
}) {
	screens = new ScreenManager({
		devices: phone,
		db,
		jar: new Uint8Array([1, 2, 3]),
		log: (event, fields = {}) => logged.push({ event, fields }),
		idleMs: options.idleMs ?? 100,
		restartDelaysMs: options.restartDelaysMs ?? [10, 10],
		statsIntervalMs: 60_000,
		wakeCheckMs: options.wakeCheckMs ?? 60_000,
		session: { connectTimeoutMs: 500, resetIntervalMs: 0 },
	});
	return screens;
}

async function until(check: () => boolean, ms = 2_000): Promise<void> {
	const deadline = Date.now() + ms;
	while (!check()) {
		if (Date.now() > deadline) throw new Error("timed out waiting");
		await Bun.sleep(5);
	}
}

// Holds every start at its video socket until the returned function runs.
function holdStarts(): () => void {
	let open = () => {};
	phone.gate = new Promise<void>((resolve) => {
		open = resolve;
	});
	return () => {
		phone.gate = null;
		open();
	};
}

const scidKills = () => phone.kills.filter((k) => k.includes("scid="));
const isConfig = (bytes: Uint8Array | undefined) =>
	((bytes?.[0] ?? 0) & 0x80) !== 0;

// The frames a viewer got: a hello before any packet, and a config packet as
// the first packet.
function expectHelloFirst(socket: FakeSocket): void {
	const hello = socket.order.indexOf("hello");
	expect(hello).toBeGreaterThanOrEqual(0);
	expect(socket.order.indexOf("binary")).toBeGreaterThan(hello);
	expect(isConfig(socket.binary[0])).toBe(true);
}

beforeEach(() => {
	phone = new FakePhone();
	logged = [];
	db = openDb(":memory:");
	db.run(
		"INSERT INTO phones (serial, site, last_state, first_seen_at) VALUES (?, 'test', 'device', '2026-10-22T00:00:00Z')",
		[SERIAL],
	);
});
afterEach(async () => {
	await screens?.stop();
	db.close();
});

test("viewers of one phone share one session; each gets hello, then a fresh config and keyframe", async () => {
	manager({});
	const first = new FakeSocket();
	screens.attach(SERIAL, first, null);
	await until(() => first.binary.length >= 2);
	expect(first.hellos()).toEqual([{ t: "hello", width: 216, height: 480 }]);
	expectHelloFirst(first);

	const second = new FakeSocket();
	screens.attach(SERIAL, second, null);
	await until(() => second.binary.length >= 2);
	expectHelloFirst(second);
	expect(phone.servers).toHaveLength(1);
	expect(phone.pushes).toBe(1);
	const control = phone.servers[0]?.control ?? [];
	// The panel is turned off right after the session starts, and each
	// viewer's keyframe comes from RESET_VIDEO: one after the wake at the
	// start, and for the joiner one at once and one after its wake.
	expect(control).toContainEqual(setDisplayPower(false));
	expect(control.filter((m) => m[0] === RESET_VIDEO[0])).toHaveLength(3);
	expect(screens.list()).toMatchObject([
		{ serial: SERIAL, state: "running", viewers: 2 },
	]);
});

test("a late joiner's first packet is a config packet, even when the encoder sends a keyframe first", async () => {
	manager({});
	const first = new FakeSocket();
	screens.attach(SERIAL, first, null);
	await until(() => first.binary.length >= 2);
	const server = phone.servers[0] as FakeServer;

	phone.answerResets = false;
	const second = new FakeSocket();
	screens.attach(SERIAL, second, null);
	await until(() => second.hellos().length === 1);
	phone.sendKeyframe(server);
	await until(() => first.binary.length >= 3);
	expect(second.binary).toEqual([]);

	phone.answerReset(server);
	await until(() => second.binary.length >= 2);
	expect(isConfig(second.binary[0])).toBe(true);
	expect(((second.binary[1]?.[0] ?? 0) & 0x40) !== 0).toBe(true);
});

test("the phone is woken and its keyguard dismissed before the panel goes dark, at start and at each join", async () => {
	manager({});
	const wake = "exec:input keyevent KEYCODE_WAKEUP; wm dismiss-keyguard";
	// After the wake, a fresh keyframe and the panel off, in that order.
	const expectWakeThenDark = (log: string[]) => {
		const woke = log.indexOf(wake);
		expect(woke).toBeGreaterThanOrEqual(0);
		const after = log.slice(woke);
		expect(after.indexOf("control:17")).toBeGreaterThan(0);
		expect(after.indexOf("control:10")).toBeGreaterThan(0);
		expect(log.slice(0, woke)).not.toContain("control:10");
	};
	screens.attach(SERIAL, new FakeSocket(), null);
	await until(() => phone.log.includes("control:10"));
	expectWakeThenDark(phone.log);

	// A viewer joining a running session wakes the phone again: it may have
	// gone to sleep while nobody was looking.
	const before = phone.log.length;
	screens.attach(SERIAL, new FakeSocket(), null);
	await until(() => phone.log.slice(before).includes("control:10"));
	expectWakeThenDark(phone.log.slice(before));
	expect(phone.servers).toHaveLength(1);
});

test("a phone that falls asleep while watched is woken once; an awake one is left alone", async () => {
	manager({ wakeCheckMs: 20 });
	const wake = "exec:input keyevent KEYCODE_WAKEUP; wm dismiss-keyguard";
	const wakes = () => phone.log.filter((entry) => entry === wake).length;
	const socket = new FakeSocket();
	const viewer = screens.attach(SERIAL, socket, null);
	await until(() => phone.log.includes("control:10"));
	// Awake: the checks run and change nothing.
	const reads = phone.wakefulnessReads;
	await until(() => phone.wakefulnessReads >= reads + 3);
	expect(wakes()).toBe(1);

	// Asleep mid-session: woken, with a fresh keyframe and the panel dark.
	phone.wakefulness = "Dozing";
	const before = phone.log.length;
	await until(() => phone.log.slice(before).includes("control:10"));
	const after = phone.log.slice(before);
	expect(after.filter((entry) => entry === wake)).toHaveLength(1);
	expect(after.indexOf(wake)).toBeLessThan(after.indexOf("control:17"));
	expect(after.indexOf("control:17")).toBeLessThan(after.indexOf("control:10"));
	await Bun.sleep(100);
	expect(wakes()).toBe(2);
	expect(socket.closed).toBeNull();

	// No checks once the session has stopped.
	viewer.detach();
	await until(() => screens.list().length === 0);
	const stopped = phone.wakefulnessReads;
	await Bun.sleep(100);
	expect(phone.wakefulnessReads).toBe(stopped);
});

test("a joiner gets a keyframe at once, before the wake finishes", async () => {
	manager({});
	screens.attach(SERIAL, new FakeSocket(), null);
	await until(() => phone.log.includes("control:10"));
	const before = phone.log.length;
	screens.attach(SERIAL, new FakeSocket(), null);
	await until(() => phone.log.slice(before).includes("control:10"));
	expect(phone.log.slice(before)[0]).toBe("control:17");
});

test("nobody watching during the idle window: the phone is left to sleep", async () => {
	manager({ wakeCheckMs: 20, idleMs: 400 });
	const viewer = screens.attach(SERIAL, new FakeSocket(), null);
	await until(() => phone.log.includes("control:10"));
	await until(() => phone.wakefulnessReads >= 2);
	viewer.detach();
	phone.wakefulness = "Dozing";
	const reads = phone.wakefulnessReads;
	await Bun.sleep(200);
	expect(screens.list()[0]?.state).toBe("running");
	expect(wakes()).toBe(1);
	expect(phone.wakefulnessReads).toBe(reads);
	expect(events("screen-woke")).toEqual([]);
});

test("after a wake the next check darkens the panel once more", async () => {
	manager({ wakeCheckMs: 20 });
	screens.attach(SERIAL, new FakeSocket(), null);
	await until(() => phone.log.includes("control:10"));
	phone.wakefulness = "Dozing";
	const before = phone.log.length;
	await until(
		() => phone.log.slice(before).filter((e) => e === "control:10").length >= 2,
	);
	const after = phone.log.slice(before);
	expect(after.filter((e) => e === WAKE)).toHaveLength(1);
	// Once: later checks find the phone awake and write nothing.
	await Bun.sleep(150);
	expect(
		phone.log.slice(before).filter((e) => e === "control:10"),
	).toHaveLength(2);
});

test("a wake that does not stick is retried, logged once, then warned about every 6th check", async () => {
	manager({ wakeCheckMs: 10 });
	screens.attach(SERIAL, new FakeSocket(), null);
	await until(() => phone.log.includes("control:10"));
	phone.wakeSticks = false;
	phone.wakefulness = "Dozing";
	await until(() => wakes() >= 13);
	expect(events("screen-woke")).toHaveLength(1);
	expect(events("screen-woke")[0]?.fields).toMatchObject({ was: "Dozing" });
	expect(events("screen-wake-not-sticking").length).toBeGreaterThanOrEqual(2);
	expect(events("screen-wake-not-sticking")[0]?.fields).toMatchObject({
		checks: 6,
	});
	// Awake again: the next time it sleeps is a new event.
	phone.wakeSticks = true;
	await until(() => phone.wakefulness === "Awake");
	await Bun.sleep(50);
	phone.wakefulness = "Dozing";
	await until(() => events("screen-woke").length === 2);
});

test("a failed wake is logged and the session carries on, with a keyframe and the panel dark", async () => {
	manager({});
	phone.wakeFails = true;
	const socket = new FakeSocket();
	screens.attach(SERIAL, socket, null);
	await until(() => phone.log.includes("control:10"));
	expect(events("screen-wake-failed")).toHaveLength(1);
	const failed = phone.log.indexOf(WAKE);
	expect(phone.log.indexOf("control:17")).toBeGreaterThan(failed);
	expect(phone.log.indexOf("control:10")).toBeGreaterThan(failed);
	expect(screens.list()[0]?.state).toBe("running");
	await until(() => socket.binary.length >= 2);
	expect(socket.closed).toBeNull();
});

test("a session stops only after the last viewer has been gone for the idle time", async () => {
	manager({ idleMs: 150 });
	const a = new FakeSocket();
	const b = new FakeSocket();
	const viewerA = screens.attach(SERIAL, a, null);
	const viewerB = screens.attach(SERIAL, b, null);
	await until(() => screens.list()[0]?.state === "running");
	viewerA.detach();
	viewerB.detach();
	await Bun.sleep(75);
	expect(phone.servers[0]?.alive).toBe(true);

	// Coming back within the idle time keeps the same server.
	const c = new FakeSocket();
	const viewerC = screens.attach(SERIAL, c, null);
	await Bun.sleep(150);
	expect(phone.servers).toHaveLength(1);
	expect(c.hellos()).toHaveLength(1);

	viewerC.detach();
	await until(() => phone.servers[0]?.alive === false, 1_000);
	await until(() => screens.list().length === 0);
	expect(scidKills()).toHaveLength(1);
});

test("a viewer that leaves and one that joins while a start is in flight share that start", async () => {
	manager({});
	const release = holdStarts();
	const first = screens.attach(SERIAL, new FakeSocket(), null);
	await until(() => phone.servers.length === 1);
	first.detach();
	const second = new FakeSocket();
	screens.attach(SERIAL, second, null);
	release();
	await until(() => second.binary.length >= 2);
	expectHelloFirst(second);
	expect(phone.servers).toHaveLength(1);
});

test("the last viewer leaving during a start stops the session after the idle time", async () => {
	manager({ idleMs: 50 });
	const release = holdStarts();
	const viewer = screens.attach(SERIAL, new FakeSocket(), null);
	await until(() => phone.servers.length === 1);
	viewer.detach();
	release();
	await until(() => screens.list().length === 0);
	expect(phone.servers[0]?.alive).toBe(false);
	expect(phone.servers).toHaveLength(1);
});

test("stop() resolves only after every server has been killed, including ones with viewers", async () => {
	manager({});
	const socket = new FakeSocket();
	screens.attach(SERIAL, socket, null);
	await until(() => socket.hellos().length === 1);
	phone.killDelayMs = 50;
	await screens.stop();
	expect(phone.killsDone.filter((k) => k.includes("scid="))).toHaveLength(1);
	expect(socket.closed?.code).toBe(CloseCode.shuttingDown);
});

test("a start kills an orphan server and waits for its CleanUp before pushing the jar", async () => {
	manager({});
	phone.orphan = { cleanupMs: 150 };
	const began = Date.now();
	const socket = new FakeSocket();
	screens.attach(SERIAL, socket, null);
	await until(() => phone.pushes === 1);
	expect(Date.now() - began).toBeGreaterThanOrEqual(140);
	expect(phone.kills[0]).toBe(
		"pkill -f 'com.genymobile.scrcpy.Serve[r]' || true",
	);
	await until(() => socket.hellos().length === 1);
});

test("a start fails when scrcpy processes do not go away", async () => {
	manager({ restartDelaysMs: [] });
	phone.orphan = { unkillable: true, cleanupMs: 0 };
	const socket = new FakeSocket();
	screens.attach(SERIAL, socket, null);
	await until(() => socket.closed !== null, 4_500);
	expect(socket.closed?.code).toBe(CloseCode.sessionFailed);
	expect(socket.closed?.reason).toContain("still running");
	expect(phone.pushes).toBe(0);
	expect(phone.servers).toEqual([]);
}, 6_000);

test("allowed input is forwarded and audited; the rest is dropped and counted", async () => {
	manager({});
	const socket = new FakeSocket();
	const handle = screens.attach(SERIAL, socket, "someone@example.com");
	await until(() => socket.hellos().length === 1);
	const server = phone.servers[0] as FakeServer;
	await until(() => server.control.some((m) => m[0] === 10));
	const before = server.control.length;

	const home = new Uint8Array(14);
	home[0] = 0;
	home[5] = 3; // KEYCODE_HOME
	const text = new Uint8Array([1, 0, 0, 0, 2, 0x68, 0x69]); // "hi"
	const back = new Uint8Array([4, 0]); // BACK_OR_SCREEN_ON, down
	expect(handle.control(home)).toBe(true);
	expect(handle.control(text)).toBe(true);
	expect(handle.control(back)).toBe(true);
	// The relay keeps the panel dark itself; viewers cannot light it.
	expect(handle.control(setDisplayPower(true))).toBe(false);
	expect(handle.control(new Uint8Array([9, 0]))).toBe(false); // clipboard
	expect(handle.control(new Uint8Array([11]))).toBe(false); // rotate
	await until(() => server.control.length === before + 3);
	expect(server.control.slice(before)).toEqual([home, text, back]);

	handle.detach();
	const rows = db.query("SELECT * FROM control_sessions").all();
	expect(rows).toEqual([
		expect.objectContaining({
			login: "someone@example.com",
			inputs: 2,
			texts: 1,
			rejected: 3,
		}),
	]);
	expect(JSON.stringify(rows)).not.toContain("hi");
	expect((rows[0] as { ended_at: string | null }).ended_at).not.toBeNull();
});

test("a crashed server is restarted for its viewers, who are closed once retries run out", async () => {
	manager({ restartDelaysMs: [10, 10] });
	const socket = new FakeSocket();
	screens.attach(SERIAL, socket, null);
	await until(() => socket.hellos().length === 1);

	phone.crash(phone.servers[0] as FakeServer);
	await until(() => socket.hellos().length === 2);
	expect(socket.text).toContainEqual({ t: "status", state: "restarting" });
	expect(phone.servers).toHaveLength(2);

	phone.crash(phone.servers[1] as FakeServer);
	await until(() => socket.hellos().length === 3);
	phone.crash(phone.servers[2] as FakeServer);
	await until(() => socket.closed !== null);
	expect(socket.closed?.code).toBe(CloseCode.sessionFailed);
	await until(() => screens.list().length === 0);
});

test("unplugging the phone closes its viewers and refuses new ones", async () => {
	manager({});
	const socket = new FakeSocket();
	screens.attach(SERIAL, socket, null);
	await until(() => socket.hellos().length === 1);
	phone.unplug(SERIAL);
	expect(socket.closed?.code).toBe(CloseCode.phoneOffline);
	await until(() => screens.list().length === 0);
	expect(() => screens.attach(SERIAL, new FakeSocket(), null)).toThrow(
		"not online",
	);
	expect(db.query("SELECT ended_at FROM control_sessions").get()).not.toEqual({
		ended_at: null,
	});
});

test("a shutting-down manager refuses viewers with close code 1001", async () => {
	manager({});
	await screens.stop();
	try {
		screens.attach(SERIAL, new FakeSocket(), null);
		throw new Error("expected attach to throw");
	} catch (error) {
		expect(error).toMatchObject({ code: CloseCode.shuttingDown });
	}
});
