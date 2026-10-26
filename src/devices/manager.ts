import type { Database } from "bun:sqlite";
import type {
	AdbStream,
	CallOptions,
	DeviceEntry,
	ShellResult,
} from "../adb/client";
import type { DeviceState } from "../adb/device-list";
import { shellV2Stdout } from "../adb/protocol";
import type { Bus } from "../bus";
import {
	applyIdentity,
	type DeviceEventRow,
	insertEvents,
	type NewEvent,
	type PhoneStatus,
	phoneStatus,
	recordPhoneState,
} from "../inventory/sync";
import type { Log } from "../log";
import {
	type DeviceShell,
	type PhoneHealth,
	type PhoneIdentity,
	probeHealth,
	probeIdentity,
} from "./probe";

// What DeviceManager needs from the adb server. AdbClient implements it.
export type AdbPort = DeviceShell & {
	version(options?: CallOptions): Promise<number>;
	trackDevices(signal?: AbortSignal): AsyncIterable<DeviceEntry[]>;
	exec(
		serial: string,
		command: string,
		options?: CallOptions,
	): Promise<Uint8Array>;
	openLocal(
		serial: string,
		name: string,
		options?: CallOptions,
	): Promise<AdbStream>;
	openStream(
		serial: string,
		service: string,
		options?: CallOptions,
	): Promise<AdbStream>;
	push(
		serial: string,
		data: Uint8Array,
		remotePath: string,
		options?: CallOptions & { mode?: number },
	): Promise<void>;
};

export type LivePhone = {
	serial: string;
	adbState: DeviceState | "gone";
	status: PhoneStatus;
	// When status last changed.
	since: string;
	usb: string | null;
	identity: PhoneIdentity | null;
	identityAt: string | null;
	health: PhoneHealth | null;
	healthAt: string | null;
	probeError: string | null;
};

export type AdbStatus = {
	reachable: boolean;
	version: number | null;
	since: string;
	error: string | null;
};

// "ready": a phone's first identity read of a session is done (applied, or
// given up after its retries), so its SIM placements are as current as they
// will get. Consumers that map sub ids to SIMs start here, not at "connected".
export type DeviceEvent =
	| "connected"
	| "ready"
	| "disconnected"
	| "subsChanged";

// The interface the rest of the app uses for phones (architecture.md, module map).
export type Devices = {
	list(): LivePhone[];
	get(serial: string): LivePhone | undefined;
	adb(): AdbStatus;
	on(event: DeviceEvent, listener: (serial: string) => void): () => void;
	// stdout, stderr, and exit code separately; for text output.
	shell(
		serial: string,
		command: string,
		options?: CallOptions,
	): Promise<ShellResult>;
	// Raw stdout only; binary-safe.
	exec(
		serial: string,
		command: string,
		options?: CallOptions,
	): Promise<Uint8Array>;
	openLocal(
		serial: string,
		name: string,
		options?: CallOptions,
	): Promise<AdbStream>;
	push(
		serial: string,
		local: Uint8Array | string,
		remote: string,
		options?: { mode?: number },
	): Promise<void>;
	// A long-running command's output as a stream, such as `logcat`. It ends
	// when the command exits, the phone goes offline, or `close` is called;
	// `timeoutMs` bounds only the setup.
	openShell(
		serial: string,
		command: string,
		options?: CallOptions,
	): Promise<AdbStream>;
	// Re-reads the phone's identity (SIMs, numbers) now instead of at the next
	// health round, for example after a SIM change.
	refresh(serial: string): void;
};

export type DeviceManagerOptions = {
	adb: AdbPort;
	db: Database;
	site: string;
	events: Bus<DeviceEventRow>;
	log: Log;
	healthIntervalMs?: number;
	// Identity is re-read this often even without a SIM state change.
	identityIntervalMs?: number;
	// Retry delays for the first identity probe after a phone comes online,
	// while its telephony stack may still be starting.
	identityRetryMs?: number[];
	maxBackoffMs?: number;
	now?: () => Date;
};

type PhoneRuntime = LivePhone & {
	// Aborted when the phone leaves "device", cancelling its probes.
	session: AbortController | null;
	probing: Promise<void> | null;
	// The session the latest scheduled probe belongs to.
	probingSession: AbortController | null;
	// An identity refresh is waiting behind the running probe.
	refreshQueued: boolean;
};

type ProbeReason = "connect" | "health" | "identity";

// The single owner of device state: it holds the one track-devices stream,
// probes phones, writes device facts to the database, and is the only code
// that opens per-phone sessions.
export class DeviceManager implements Devices {
	private readonly phones = new Map<string, PhoneRuntime>();
	private readonly listeners = new Map<
		DeviceEvent,
		Set<(serial: string) => void>
	>();
	private readonly stopping = new AbortController();
	private adbStatus: AdbStatus;
	private adbKnown = false;
	private healthTimer: Timer | undefined;
	private loop: Promise<void> | undefined;
	private readonly healthIntervalMs: number;
	private readonly identityIntervalMs: number;
	private readonly identityRetryMs: number[];
	private readonly maxBackoffMs: number;
	private readonly now: () => Date;

	constructor(private readonly options: DeviceManagerOptions) {
		this.healthIntervalMs = options.healthIntervalMs ?? 60_000;
		this.identityIntervalMs = options.identityIntervalMs ?? 30 * 60_000;
		this.identityRetryMs = options.identityRetryMs ?? [5_000, 15_000, 45_000];
		this.maxBackoffMs = options.maxBackoffMs ?? 15_000;
		this.now = options.now ?? (() => new Date());
		this.adbStatus = {
			reachable: false,
			version: null,
			since: this.iso(),
			error: "not connected yet",
		};
	}

	start(): void {
		this.loop ??= this.track();
		this.healthTimer ??= setInterval(() => this.tick(), this.healthIntervalMs);
	}

	async stop(): Promise<void> {
		this.stopping.abort();
		clearInterval(this.healthTimer);
		for (const phone of this.phones.values()) phone.session?.abort();
		await this.loop;
	}

	list(): LivePhone[] {
		return [...this.phones.values()].map(publicView);
	}

	get(serial: string): LivePhone | undefined {
		const phone = this.phones.get(serial);
		return phone && publicView(phone);
	}

	adb(): AdbStatus {
		return { ...this.adbStatus };
	}

	on(event: DeviceEvent, listener: (serial: string) => void): () => void {
		let set = this.listeners.get(event);
		if (!set) {
			set = new Set();
			this.listeners.set(event, set);
		}
		set.add(listener);
		return () => set.delete(listener);
	}

	async shell(
		serial: string,
		command: string,
		options: CallOptions = {},
	): Promise<ShellResult> {
		return this.options.adb.shell(
			serial,
			command,
			this.sessionOptions(serial, options),
		);
	}

	async exec(
		serial: string,
		command: string,
		options: CallOptions = {},
	): Promise<Uint8Array> {
		return this.options.adb.exec(
			serial,
			command,
			this.sessionOptions(serial, options),
		);
	}

	async openLocal(
		serial: string,
		name: string,
		options: CallOptions = {},
	): Promise<AdbStream> {
		return this.options.adb.openLocal(
			serial,
			name,
			this.sessionOptions(serial, options),
		);
	}

	async push(
		serial: string,
		local: Uint8Array | string,
		remote: string,
		options: { mode?: number } = {},
	): Promise<void> {
		const data =
			typeof local === "string"
				? new Uint8Array(await Bun.file(local).arrayBuffer())
				: local;
		await this.options.adb.push(serial, data, remote, {
			...this.sessionOptions(serial, {}),
			...options,
			timeoutMs: 120_000,
		});
	}

	async openShell(
		serial: string,
		command: string,
		options: CallOptions = {},
	): Promise<AdbStream> {
		// The shell protocol (v2), unlike a plain shell stream, has adbd end
		// the command when the stream closes.
		const stream = await this.options.adb.openStream(
			serial,
			`shell,v2,raw:${command}`,
			this.sessionOptions(serial, options),
		);
		return {
			...stream,
			readable: stream.readable.pipeThrough(shellV2Stdout()),
		};
	}

	refresh(serial: string): void {
		const phone = this.phones.get(serial);
		if (phone?.status === "online") this.schedule(phone, "identity");
	}

	// Waits for any probes in flight; for tests and shutdown.
	async settled(): Promise<void> {
		while ([...this.phones.values()].some((phone) => phone.probing))
			await Promise.all(
				[...this.phones.values()].map((phone) => phone.probing),
			);
	}

	// Runs the health round now instead of at the next interval.
	tick(): void {
		for (const phone of this.phones.values()) {
			if (phone.status !== "online" || phone.probing) continue;
			this.schedule(phone, "health");
		}
	}

	private sessionOptions(serial: string, options: CallOptions): CallOptions {
		const phone = this.phones.get(serial);
		if (phone?.status !== "online" || !phone.session)
			throw new Error(
				`phone ${serial} is not online (${phone?.adbState ?? "unknown"})`,
			);
		const signals = [phone.session.signal, options.signal].filter(
			(signal): signal is AbortSignal => !!signal,
		);
		return { ...options, signal: AbortSignal.any(signals) };
	}

	private async track(): Promise<void> {
		const signal = this.stopping.signal;
		let attempt = 0;
		while (!signal.aborted) {
			try {
				const version = await this.options.adb.version({
					signal,
					timeoutMs: 5_000,
				});
				this.setAdb({ reachable: true, version, error: null });
				for await (const list of this.options.adb.trackDevices(signal)) {
					attempt = 0;
					this.applyList(list);
				}
				throw new Error("the adb server closed the device stream");
			} catch (error) {
				if (signal.aborted) break;
				this.setAdb({
					reachable: false,
					version: null,
					error: errorText(error),
				});
				// Without the adb server no phone is reachable, including ones the
				// database last saw online before this process started.
				this.markUnseenGone(new Set());
			}
			const delay = Math.min(this.maxBackoffMs, 500 * 2 ** attempt++);
			await sleep(delay, signal);
		}
	}

	private setAdb(next: Omit<AdbStatus, "since">): void {
		const first = !this.adbKnown;
		this.adbKnown = true;
		const changed = first || next.reachable !== this.adbStatus.reachable;
		this.adbStatus = {
			...next,
			since: changed ? this.iso() : this.adbStatus.since,
		};
		// Reaching the server at startup is not news; failing to, or recovering, is.
		if (!changed || (first && next.reachable)) return;
		this.options.log(next.reachable ? "adb-reachable" : "adb-unreachable", {
			version: next.version,
			error: next.error,
		});
		this.publish([
			{
				phoneId: null,
				kind: next.reachable ? "adb.reachable" : "adb.unreachable",
				detail: { error: next.error },
			},
		]);
	}

	private applyList(list: DeviceEntry[]): void {
		for (const entry of list) this.applyState(entry.serial, entry.state, entry);
		this.markUnseenGone(new Set(list.map((entry) => entry.serial)));
	}

	// Marks every phone not in `seen` as gone: phones this run knows, and phones
	// the database last saw settled (online, waiting for auth), which after a
	// restart would otherwise keep a stale state.
	private markUnseenGone(seen: Set<string>): void {
		for (const phone of this.phones.values())
			if (!seen.has(phone.serial)) this.applyState(phone.serial, "gone", null);
		const stale = this.options.db
			.query<{ serial: string }, []>(
				"SELECT serial FROM phones WHERE last_state NOT IN ('gone', 'offline')",
			)
			.all()
			.filter((row) => !seen.has(row.serial) && !this.phones.has(row.serial));
		for (const row of stale) this.applyState(row.serial, "gone", null);
	}

	private applyState(
		serial: string,
		state: DeviceState | "gone",
		entry: DeviceEntry | null,
	): void {
		let phone = this.phones.get(serial);
		if (phone?.adbState === state) return;
		if (!phone) {
			phone = {
				serial,
				adbState: state,
				status: "offline",
				since: this.iso(),
				usb: null,
				identity: null,
				identityAt: null,
				health: null,
				healthAt: null,
				probeError: null,
				session: null,
				probing: null,
				probingSession: null,
				refreshQueued: false,
			};
			this.phones.set(serial, phone);
		}
		const before = phone.status;
		phone.adbState = state;
		phone.status = phoneStatus(state);
		if (entry?.usb) phone.usb = entry.usb;
		if (phone.status !== before) phone.since = this.iso();

		const { phoneId, events } = recordPhoneState(this.options.db, {
			serial,
			state,
			site: this.options.site,
			at: this.iso(),
			detail: entry ? { rawState: entry.rawState } : {},
		});
		this.publish(events.map((event) => ({ ...event, phoneId })));
		this.options.log("phone-state", { serial, state });

		if (phone.status === "online" && !phone.session) {
			phone.session = new AbortController();
			phone.probeError = null;
			this.schedule(phone, "connect");
			this.emit("connected", serial);
		} else if (phone.status !== "online" && phone.session) {
			phone.session.abort();
			phone.session = null;
			this.emit("disconnected", serial);
		}
	}

	// One probe at a time per phone. A probe for a new session (the phone came
	// back while the old session's probe was still unwinding) runs after the
	// old one instead of being dropped.
	private schedule(phone: PhoneRuntime, reason: ProbeReason): void {
		const session = phone.session;
		if (!session) return;
		// A refresh asked for while a probe runs follows it, since that probe
		// may have read the SIMs before the change.
		if (
			phone.probing &&
			phone.probingSession === session &&
			(reason !== "identity" || phone.refreshQueued)
		)
			return;
		if (reason === "identity" && phone.probing) phone.refreshQueued = true;
		const previous = phone.probing ?? Promise.resolve();
		const run = previous
			.then(() => {
				if (reason === "identity") phone.refreshQueued = false;
				return session.signal.aborted
					? undefined
					: this.probe(phone, reason, session.signal);
			})
			.catch((error: unknown) => {
				if (session.signal.aborted) return;
				phone.probeError = errorText(error);
				this.options.log("probe-failed", {
					serial: phone.serial,
					reason,
					error: phone.probeError,
				});
			})
			.finally(() => {
				if (phone.probing === run) {
					phone.probing = null;
					phone.probingSession = null;
				}
			});
		phone.probing = run;
		phone.probingSession = session;
	}

	private async probe(
		phone: PhoneRuntime,
		reason: ProbeReason,
		signal: AbortSignal,
	): Promise<void> {
		const adb = this.options.adb;
		if (reason === "identity") {
			await this.readIdentity(phone, signal);
			return;
		}
		if (reason === "connect") {
			try {
				for (let attempt = 0; ; attempt++) {
					try {
						await this.readIdentity(phone, signal);
						break;
					} catch (error) {
						const delay = this.identityRetryMs[attempt];
						if (delay === undefined || signal.aborted) throw error;
						this.options.log("identity-retry", {
							serial: phone.serial,
							error: errorText(error),
							delayMs: delay,
						});
						await sleep(delay, signal);
					}
				}
			} finally {
				if (!signal.aborted) this.emit("ready", phone.serial);
			}
		}
		if (signal.aborted) return;
		const health = await probeHealth(adb, phone.serial, {
			signal,
			timeoutMs: 20_000,
		});
		if (signal.aborted) return;
		phone.health = health;
		phone.healthAt = this.iso();
		phone.probeError = null;
		const identitySims = phone.identity?.slots
			.map((slot) => slot.simState ?? "")
			.join(",");
		const due =
			reason === "health" &&
			(phone.identity === null ||
				identitySims !== health.simStates.join(",") ||
				Date.parse(phone.identityAt ?? "") + this.identityIntervalMs <=
					this.now().getTime());
		if (due) await this.readIdentity(phone, signal);
	}

	private async readIdentity(
		phone: PhoneRuntime,
		signal: AbortSignal,
	): Promise<void> {
		const identity = await probeIdentity(this.options.adb, phone.serial, {
			signal,
			timeoutMs: 20_000,
		});
		if (signal.aborted) return;
		phone.identity = identity;
		phone.identityAt = this.iso();
		const phoneId = this.options.db
			.query<{ id: number }, [string]>("SELECT id FROM phones WHERE serial = ?")
			.get(phone.serial)?.id;
		if (phoneId === undefined) return;
		const events = applyIdentity(
			this.options.db,
			phoneId,
			identity,
			this.iso(),
		);
		this.publish(events);
		if (identity.warnings.length > 0)
			this.options.log("identity-warnings", {
				serial: phone.serial,
				warnings: identity.warnings,
			});
		if (
			events.some(
				(event) =>
					event.kind.startsWith("sim.") || event.kind.startsWith("number."),
			)
		)
			this.emit("subsChanged", phone.serial);
	}

	private publish(events: NewEvent[]): void {
		if (events.length === 0) return;
		for (const row of insertEvents(this.options.db, events, this.iso()))
			this.options.events.publish(row);
	}

	private emit(event: DeviceEvent, serial: string): void {
		for (const listener of this.listeners.get(event) ?? []) {
			try {
				listener(serial);
			} catch (error) {
				this.options.log("listener-failed", {
					event,
					serial,
					error: errorText(error),
				});
			}
		}
	}

	private iso(): string {
		return this.now().toISOString();
	}
}

function publicView(phone: PhoneRuntime): LivePhone {
	const {
		session: _session,
		probing: _probing,
		probingSession: _probingSession,
		refreshQueued: _refreshQueued,
		...view
	} = phone;
	return view;
}

function errorText(error: unknown): string {
	return error instanceof Error ? error.message : String(error);
}

function sleep(ms: number, signal: AbortSignal): Promise<void> {
	return new Promise((resolve) => {
		if (signal.aborted) return resolve();
		const timer = setTimeout(done, ms);
		function done() {
			clearTimeout(timer);
			signal.removeEventListener("abort", done);
			resolve();
		}
		signal.addEventListener("abort", done, { once: true });
	});
}
