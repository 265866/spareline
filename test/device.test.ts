import { describe, expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { AdbClient, parseEndpoint } from "../src/adb/client";
import { Bus } from "../src/bus";
import { openDb } from "../src/db/db";
import { isIccid, isImei } from "../src/devices/identifiers";
import { DeviceManager } from "../src/devices/manager";
import { probeHealth, probeIdentity } from "../src/devices/probe";
import { silent } from "../src/log";
import { SmsIngest } from "../src/sms/ingest";
import { parseContentRows } from "../src/sms/parse";
import { type Message, MessageStore } from "../src/sms/store";

// Runs against a real adb server and phone, read-only apart from one file in
// /data/local/tmp (removed) and a loopback-only echo server (killed):
//
//   SPARELINE_DEVICE_TESTS=1 ADB_SERVER=127.0.0.1:5037 bun test test/device.test.ts
//
// SPARELINE_DEVICE_SERIAL picks a phone; otherwise the first online one.
const enabled = process.env.SPARELINE_DEVICE_TESTS === "1";
const adb = new AdbClient(
	parseEndpoint(process.env.ADB_SERVER ?? "127.0.0.1:5037"),
);

async function serial(): Promise<string> {
	const wanted = process.env.SPARELINE_DEVICE_SERIAL;
	const online = (await adb.listDevices()).filter(
		(device) => device.state === "device",
	);
	const found = wanted
		? online.find((device) => device.serial === wanted)
		: online[0];
	if (!found)
		throw new Error(wanted ? `${wanted} is not online` : "no phone is online");
	return found.serial;
}

describe.skipIf(!enabled)("real adb server and phone", () => {
	test("version and track-devices", async () => {
		expect(await adb.version()).toBeGreaterThanOrEqual(39);
		const phone = await serial();
		const controller = new AbortController();
		for await (const list of adb.trackDevices(controller.signal)) {
			expect(list.find((device) => device.serial === phone)?.state).toBe(
				"device",
			);
			break;
		}
	});

	test("identity: model, Android version, valid IMEIs and ICCIDs", async () => {
		const identity = await probeIdentity(adb, await serial(), {
			timeoutMs: 20_000,
		});
		expect(identity.model).toBeTruthy();
		expect(identity.androidVersion).toMatch(/^\d+/);
		for (const imei of identity.imeis.filter((value) => value !== null))
			expect(isImei(imei)).toBe(true);
		for (const sim of identity.sims) {
			if (sim.iccid !== null) expect(isIccid(sim.iccid)).toBe(true);
			if (sim.number !== null) expect(sim.number).toMatch(/^\+\d{8,15}$/);
		}
		expect(identity.slots.length).toBeGreaterThan(0);
	}, 30_000);

	test("health: battery and one radio state per slot", async () => {
		const health = await probeHealth(adb, await serial(), {
			timeoutMs: 20_000,
		});
		expect(health.battery?.level).toBeGreaterThanOrEqual(0);
		expect(health.battery?.temperatureDeciC).toBeGreaterThan(0);
		expect(health.radios.length).toBe(health.simStates.length);
	}, 30_000);

	test("exec is binary-safe and push round-trips", async () => {
		const phone = await serial();
		const elf = await adb.exec(phone, "head -c 4 /system/bin/toybox");
		expect([...elf]).toEqual([0x7f, 0x45, 0x4c, 0x46]);
		const data = new Uint8Array(150_000).map((_, i) => (i * 31) % 256);
		const path = "/data/local/tmp/spareline-device-test.bin";
		await adb.push(phone, data, path);
		const back = await adb.exec(phone, `cat ${path}`);
		expect(Buffer.compare(Buffer.from(back), Buffer.from(data))).toBe(0);
		// The shell protocol (how MMS parts are read) is binary-safe too, and
		// keeps stderr and the exit status apart.
		const viaShell = await adb.shell(phone, `cat ${path}; rm -f ${path}`);
		expect(viaShell.exitCode).toBe(0);
		expect(viaShell.stderr.length).toBe(0);
		expect(
			Buffer.compare(Buffer.from(viaShell.stdout), Buffer.from(data)),
		).toBe(0);
	}, 30_000);

	test("exec: mixes stderr into its output; the shell protocol does not", async () => {
		const phone = await serial();
		const mixed = new TextDecoder().decode(
			await adb.exec(phone, "echo out; echo err >&2"),
		);
		expect(mixed).toContain("out");
		expect(mixed).toContain("err");
		const apart = await adb.shell(phone, "echo out; echo err >&2; exit 3");
		expect(new TextDecoder().decode(apart.stdout)).toBe("out\n");
		expect(new TextDecoder().decode(apart.stderr)).toBe("err\n");
		expect(apart.exitCode).toBe(3);
	}, 30_000);

	// Read-only: a part id that does not exist.
	test("content read of a missing MMS part is not mistaken for data", async () => {
		const phone = await serial();
		const result = await adb.shell(
			phone,
			"content read --uri content://mms/part/2000000000",
			{ timeoutMs: 30_000 },
		);
		const unreadable =
			result.exitCode !== 0 ||
			result.stderr.length > 0 ||
			result.stdout.length === 0;
		console.error(
			JSON.stringify({
				event: "device-missing-part",
				exitCode: result.exitCode,
				stdoutBytes: result.stdout.length,
				stderr: new TextDecoder().decode(result.stderr).split("\n")[0],
			}),
		);
		expect(unreadable).toBe(true);
	}, 30_000);

	test("duplex stream to a loopback echo server on the phone", async () => {
		const phone = await serial();
		await adb.shell(
			phone,
			"(toybox nc -s 127.0.0.1 -p 27184 -L cat </dev/null >/dev/null 2>&1 &); sleep 0.3",
		);
		try {
			const stream = await adb.openStream(phone, "tcp:27184");
			const payload = new Uint8Array(100_000).map((_, i) => i % 253);
			const reader = stream.readable.getReader();
			const received: Uint8Array[] = [];
			let total = 0;
			const reading = (async () => {
				while (total < payload.length) {
					const { value, done } = await reader.read();
					if (done) break;
					received.push(value);
					total += value.length;
				}
			})();
			await stream.write(payload);
			await reading;
			stream.close();
			expect(
				Buffer.compare(Buffer.concat(received), Buffer.from(payload)),
			).toBe(0);
		} finally {
			await adb.shell(phone, "pkill -f 'nc -s 127.0.0.1 -p 27184'");
		}
	}, 30_000);

	// Reads the inbox (no writes on the phone) into a scratch database.
	test("SMS backfill imports the provider's inbox exactly once", async () => {
		const phone = await serial();
		const db = openDb(":memory:");
		const dir = mkdtempSync(join(tmpdir(), "spareline-device-sms-"));
		const manager = new DeviceManager({
			adb,
			db,
			site: "device-test",
			events: new Bus(),
			log: silent,
		});
		try {
			manager.start();
			for (let i = 0; i < 600 && !manager.get(phone)?.identityAt; i++)
				await Bun.sleep(100);
			expect(manager.get(phone)?.identityAt).toBeTruthy();
			const store = new MessageStore(db);
			const published: Message[] = [];
			const bus = new Bus<Message>();
			bus.subscribe((message) => published.push(message));
			const ingest = new SmsIngest({
				devices: manager,
				db,
				store,
				messages: bus,
				attachmentsDir: join(dir, "attachments"),
				log: silent,
			});
			const countOf = async (uri: string, where = "") => {
				const result = await adb.shell(
					phone,
					`content query --uri ${uri} --projection 'count(*)'${where}`,
					{ timeoutMs: 30_000 },
				);
				const { rows } = parseContentRows(
					new TextDecoder().decode(result.stdout),
					["count(*)"],
				);
				return Number(rows[0]?.["count(*)"]);
			};
			const provider =
				(await countOf("content://sms/inbox")) +
				(await countOf("content://mms/inbox", " --where 'm_type=132'"));
			expect(await ingest.reconcile(phone)).toBe(provider);
			const stored = db
				.query<{ n: number; numbered: number }, []>(
					"SELECT count(*) AS n, count(number_id) AS numbered FROM messages",
				)
				.get();
			expect(stored?.n).toBe(provider);
			expect(published).toHaveLength(provider);
			// The SIM is known from the identity probe, so every text has a number.
			expect(stored?.numbered).toBe(provider);
			expect(await ingest.reconcile(phone)).toBe(0);
			expect(published).toHaveLength(provider);
			console.error(
				JSON.stringify({
					event: "device-sms-backfill",
					provider,
					stored: stored?.n,
				}),
			);
		} finally {
			await manager.stop();
			db.close();
			rmSync(dir, { recursive: true, force: true });
		}
	}, 120_000);
});
