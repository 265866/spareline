import type { ShellResult } from "../src/adb/client";
import { Bus } from "../src/bus";
import { openDb } from "../src/db/db";
import { DeviceManager } from "../src/devices/manager";
import { parseIsub, splitSections } from "../src/devices/parse";
import type { DeviceEventRow } from "../src/inventory/sync";
import { silent } from "../src/log";
import { Monitor, type MonitorOptions } from "../src/monitor/monitor";
import { LOG_BUFFER_COMMAND, READ_SCRIPT } from "../src/monitor/policy";
import { SIM_STATE_COMMAND } from "../src/monitor/sims";
import { FakeAdb, recordedTrackLists } from "./fake-adb";
import { fixture } from "./replay-shell";

// A B30 stand-in for monitor tests. It answers the policy read with the
// output recorded from the real B30 (policy-read.txt) and applies the writes
// the monitor makes (settings put, svc, cmd netpolicy, pm uninstall) to that
// recording. Drift, a SIM insertion, and data use cannot be produced on
// demand on the real phone (and turning a radio on to cause drift is
// forbidden), so tests start from the recording and change one thing.
export class MonitorPhone extends FakeAdb {
	// Every command the monitor ran through an apply script, in order.
	readonly applied: string[] = [];
	readonly reads: string[] = [];
	private sections = new Map<string, string>();
	private order: string[] = [];
	simState = "LOADED,ABSENT";
	// Settings ("global:bluetooth_on") that ignore the monitor's writes, like a
	// radio that will not turn off.
	readonly stuck = new Set<string>();
	// While set, apply scripts fail like a phone that drops off USB.
	failApply: Error | null = null;
	// Runs once, after a policy read is taken and before it is answered, to
	// order something between the two.
	duringRead: (() => Promise<void>) | null = null;

	static async recorded(): Promise<MonitorPhone> {
		const phone = await new MonitorPhone().recordB30();
		const read = await fixture("nuu-s6702x", "policy-read.txt");
		phone.sections = splitSections(read);
		phone.order = [...phone.sections.keys()];
		return phone;
	}

	section(name: string): string {
		return this.sections.get(name) ?? "";
	}

	setSection(name: string, body: string): void {
		this.sections.set(name, body);
	}

	// Changes one setting the way a person tapping in Settings would.
	set(ns: "global" | "secure" | "system", key: string, value: string | null) {
		const name = `settings:${ns}`;
		const lines = this.section(name)
			.split("\n")
			.filter((line) => line && !line.startsWith(`${key}=`));
		if (value !== null) lines.push(`${key}=${value}`);
		this.sections.set(name, lines.join("\n"));
	}

	private write(
		ns: "global" | "secure" | "system",
		key: string,
		value: string,
	) {
		if (!this.stuck.has(`${ns}:${key}`)) this.set(ns, key, value);
	}

	// A radio buffer the log_buffer rule accepts (the B30's is 256 KiB).
	bigRadioBuffer(): void {
		this.setSection(
			"radiobuf",
			"radio: ring buffer is 8 MiB (252 KiB consumed, 1 MiB readable), max entry is 5120 B, max payload is 4068 B",
		);
	}

	get(ns: "global" | "secure" | "system", key: string): string | null {
		const line = this.section(`settings:${ns}`)
			.split("\n")
			.find((candidate) => candidate.startsWith(`${key}=`));
		return line ? line.slice(key.length + 1) : null;
	}

	override async shell(serial: string, command: string): Promise<ShellResult> {
		if (this.hold) await this.hold;
		this.calls.push(`${serial}: ${command}`);
		if (command === READ_SCRIPT) {
			this.reads.push(serial);
			const output = this.order
				.map((name) => `\n@@sl:${name}\n${this.section(name)}\n`)
				.join("");
			const during = this.duringRead;
			this.duringRead = null;
			await during?.();
			return ok(output);
		}
		if (command === "dumpsys isub") return ok(this.section("isub"));
		if (command === SIM_STATE_COMMAND) return ok(`${this.simState}\n`);
		if (command.includes("@@sl:run:")) {
			if (this.failApply) throw this.failApply;
			return ok(this.apply(command));
		}
		return super.shell(serial, command);
	}

	private apply(script: string): string {
		const commands = [
			...script.matchAll(/@@sl:run:\d+\\n'; (.*?) 2>&1; echo "rc=\$\?"/g),
		].map((match) => match[1] ?? "");
		let out = "";
		commands.forEach((command, index) => {
			this.applied.push(command);
			this.effect(command);
			out += `\n@@sl:run:${index}\nrc=0\n`;
		});
		return out;
	}

	private effect(command: string): void {
		const put = /^settings put (global|secure|system) (\S+) (\S+)$/.exec(
			command,
		);
		if (put) {
			this.write(put[1] as "global", put[2] ?? "", put[3] ?? "");
			return;
		}
		// svc data disable turns data off for the default data SIM, which on
		// Android 14 writes both mobile_data and that SIM's mobile_data<subId>
		// (TelephonyManager.setDataEnabled through DataSettingsManager, read).
		// Other SIMs' keys stay as they were.
		if (command === "svc data disable") {
			this.write("global", "mobile_data", "0");
			const sub = parseIsub(this.section("isub")).defaultDataSubId;
			if (sub !== null && sub >= 0)
				this.write("global", `mobile_data${sub}`, "0");
			return;
		}
		const svc: Record<string, string> = {
			"svc wifi disable": "wifi_on",
			"cmd bluetooth_manager disable": "bluetooth_on",
		};
		const key = svc[command];
		if (key) {
			this.write("global", key, "0");
			return;
		}
		if (command === "cmd bluetooth_manager wait-for-state:STATE_OFF") return;
		if (command === LOG_BUFFER_COMMAND) {
			if (!this.stuck.has("logd")) this.bigRadioBuffer();
			return;
		}
		if (command === "cmd netpolicy set restrict-background true") {
			this.sections.set("datasaver", "Restrict background status: enabled");
			return;
		}
		const uninstall = /^pm uninstall --user 0 (\S+)$/.exec(command);
		if (uninstall) {
			this.sections.set(
				"bloat",
				this.section("bloat")
					.split("\n")
					.filter((line) => line !== `package:${uninstall[1]}`)
					.join("\n"),
			);
			return;
		}
		if (command.startsWith("cmd role add-role-holder ")) return;
		throw new Error(`MonitorPhone: no effect recorded for "${command}"`);
	}
}

function ok(stdout: string): ShellResult {
	return {
		stdout: new TextEncoder().encode(stdout),
		stderr: new Uint8Array(),
		exitCode: 0,
	};
}

// The recorded netstats plus one app's traffic on the metered internet APN
// of sub 1, in the per-uid section's recorded format: mobile data stays off
// on the real phone, so internet traffic cannot be recorded.
export function withMeteredTraffic(
	netstats: string,
	options: { bytes: number; now: number; uid?: number },
): string {
	const rows = [
		`  ident=[{type=0, ratType=13, subscriberId=310260..., metered=true, defaultNetwork=true, oemManaged=OEM_NONE, subId=1}] uid=${options.uid ?? 10101} set=DEFAULT tag=0x0`,
		"    NetworkStatsHistory: bucketDuration=7200",
		`      st=${options.now - 3600} rb=${options.bytes} rp=900 tb=0 tp=0 op=0`,
	];
	const lines = netstats.split("\n");
	lines.splice(
		lines.findIndex((line) => line.startsWith("InterfaceMapUpdater:")),
		0,
		...rows,
	);
	return lines.join("\n");
}

type Options = Partial<Omit<MonitorOptions, "devices" | "db" | "events">> & {
	// Changes the phone before it comes online.
	prepare?: (phone: MonitorPhone) => void;
};

// A DeviceManager and Monitor over a MonitorPhone, with the B30 online.
export async function startMonitor(options: Options = {}) {
	const db = openDb(":memory:");
	const phone = await MonitorPhone.recorded();
	const events: DeviceEventRow[] = [];
	const bus = new Bus<DeviceEventRow>();
	bus.subscribe((event) => events.push(event));
	const clock = { now: new Date("2026-10-22T08:00:00.000Z") };
	const now = () => clock.now;
	const devices = new DeviceManager({
		adb: phone,
		db,
		site: "home",
		events: bus,
		log: silent,
		healthIntervalMs: 3_600_000,
		identityRetryMs: [],
		now,
	});
	const monitor = new Monitor({
		devices,
		db,
		events: bus,
		log: silent,
		checkIntervalMs: 3_600_000,
		healthIntervalMs: 3_600_000,
		simPollMs: 3_600_000,
		simDebounceMs: 5,
		now,
		...options,
	});
	options.prepare?.(phone);
	monitor.start();
	devices.start();
	phone.send((await recordedTrackLists())[0] ?? []);
	await until(() => monitor.phone(B30)?.check != null, "first monitor run");
	await devices.settled();
	await monitor.settled();
	// What the connect check wrote, then a clean slate for the test.
	const atConnect = phone.applied.splice(0);
	const readsAtConnect = phone.reads.splice(0).length;
	return {
		atConnect,
		readsAtConnect,
		db,
		phone,
		events,
		bus,
		clock,
		devices,
		monitor,
		kinds: () => events.map((event) => event.kind),
		stop: async () => {
			await monitor.stop();
			await devices.stop();
			db.close();
		},
	};
}

export const B30 = "SPARELINE00001";

export async function until(check: () => boolean, what: string): Promise<void> {
	for (let i = 0; i < 400; i++) {
		if (check()) return;
		await Bun.sleep(5);
	}
	throw new Error(`timed out waiting for ${what}`);
}
