import type { AdbStream } from "../adb/client";
import type { Devices } from "../devices/manager";
import type { Log } from "../log";

// Radio log lines that mean a SIM card changed state. On the B30's user
// build the radio buffer carries the RIL's own lines (measured: RILJ
// requests as `[1234]> NAME`, unsolicited ones as `[UNSL]< NAME` or
// `[UNSL]< MTK: RIL_UNSOL_...`, and the modem's AT traffic), not the Java
// UiccController logs. These names come from AOSP's RIL
// (UNSOL_RESPONSE_SIM_STATUS_CHANGED, GET_SIM_STATUS, slot status) and MTK's
// (RIL_UNSOL_SIM_PLUG_IN/OUT, SIM_MISSING, SIM_RECOVERY, COMMON_SLOT, and
// the +ESIMS hot-plug URC). None appeared while SIM state was only queried,
// and a swap cannot be produced on demand, so they are unconfirmed on the
// B30; the 10 s gsm.sim.state poll is the backstop. A false hit only costs
// one extra `dumpsys isub`.
export const SIM_LOG_PATTERN =
	"SIM_STATUS|SIM_PLUG|SIM_MISSING|SIM_RECOVERY|SIM_COMMON_SLOT|SLOT_STATUS|ICC_STATUS|[+]ESIMS";

// -T 1 starts at the newest line instead of replaying the buffer.
export const SIM_LOG_COMMAND = `logcat -b radio -v brief -T 1 -e '${SIM_LOG_PATTERN}'`;

export const SIM_STATE_COMMAND = "getprop gsm.sim.state";

// Which subscription is in each slot (active subscriptions only).
export type SlotMap = ReadonlyMap<number, number>;

export type SimChange = {
	slot: number;
	kind: "inserted" | "removed" | "swapped";
	from: number | null;
	to: number | null;
};

// Android gives each ICCID its own subscription id and reuses it when the
// same card returns (read: SubscriptionManagerService), so a slot whose sub
// id changes holds a different card.
export function diffSlots(before: SlotMap, after: SlotMap): SimChange[] {
	const slots = [...new Set([...before.keys(), ...after.keys()])].sort(
		(a, b) => a - b,
	);
	const changes: SimChange[] = [];
	for (const slot of slots) {
		const from = before.get(slot) ?? null;
		const to = after.get(slot) ?? null;
		if (from === to) continue;
		changes.push({
			slot,
			kind: from === null ? "inserted" : to === null ? "removed" : "swapped",
			from,
			to,
		});
	}
	return changes;
}

export type SimSignal = "logcat" | "sim_state";

// Watches one online phone for SIM changes: a filtered radio log stream,
// plus a gsm.sim.state poll in case the stream misses one. Both only signal;
// the monitor re-reads isub and decides.
export class SimWatch {
	private running = false;
	private stream: AdbStream | null = null;
	private timer: Timer | undefined;
	private polling = false;
	private lastState: string | null = null;

	constructor(
		private readonly options: {
			devices: Devices;
			serial: string;
			onSignal: (reason: SimSignal) => void;
			log: Log;
			pollMs: number;
			// Wait before reopening the log stream after it ends.
			retryMs: number;
		},
	) {}

	start(): void {
		if (this.running) return;
		this.running = true;
		void this.follow();
		this.timer = setInterval(() => void this.poll(), this.options.pollMs);
		void this.poll();
	}

	stop(): void {
		this.running = false;
		clearInterval(this.timer);
		this.stream?.close();
		this.stream = null;
	}

	private async follow(): Promise<void> {
		const { devices, serial, log } = this.options;
		while (this.running) {
			try {
				const stream = await devices.openShell(serial, SIM_LOG_COMMAND, {
					timeoutMs: 10_000,
				});
				if (!this.running) {
					stream.close();
					return;
				}
				this.stream = stream;
				const reader = stream.readable.getReader();
				const decoder = new TextDecoder();
				let pending = "";
				for (;;) {
					const { value, done } = await reader.read();
					if (done) break;
					pending += decoder.decode(value, { stream: true });
					const lines = pending.split("\n");
					pending = lines.pop() ?? "";
					// The line itself is not logged: it can carry identifiers.
					if (lines.some((line) => line.trim() && !line.startsWith("-----")))
						this.options.onSignal("logcat");
				}
			} catch (error) {
				if (!this.running) return;
				log("sim-log-failed", {
					serial,
					error: error instanceof Error ? error.message : String(error),
				});
			}
			this.stream = null;
			if (!this.running) return;
			await Bun.sleep(this.options.retryMs);
		}
	}

	private async poll(): Promise<void> {
		if (this.polling || !this.running) return;
		this.polling = true;
		try {
			const result = await this.options.devices.shell(
				this.options.serial,
				SIM_STATE_COMMAND,
				{ timeoutMs: 5_000 },
			);
			const state = new TextDecoder().decode(result.stdout).trim();
			if (this.lastState !== null && state !== this.lastState)
				this.options.onSignal("sim_state");
			this.lastState = state;
		} catch {
			// The phone went away; the monitor stops this watch on disconnect.
		} finally {
			this.polling = false;
		}
	}
}
