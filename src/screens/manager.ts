import type { Database } from "bun:sqlite";
import type { Devices } from "../devices/manager";
import type { Log } from "../log";
import {
	classifyControl,
	parseWakefulness,
	setDisplayPower,
	type VideoPacket,
	WAKE_COMMAND,
	WAKEFULNESS_COMMAND,
} from "./protocol";
import {
	ScrcpySession,
	type ScreenDevices,
	type SessionOptions,
} from "./session";
import { BACKLOG_LIMIT, Viewer, type ViewerSink } from "./viewer";

export type ScreenManagerOptions = {
	devices: ScreenDevices & Pick<Devices, "get" | "on">;
	db: Database;
	jar: Uint8Array;
	log: Log;
	// A session stops this long after its last viewer leaves.
	idleMs?: number;
	// Delays before restarting a session that died with viewers attached.
	restartDelaysMs?: number[];
	statsIntervalMs?: number;
	// How often a running session checks that its phone is still awake.
	wakeCheckMs?: number;
	session?: SessionOptions;
};

// What the relay tells a viewer, as JSON text frames. Binary frames are
// scrcpy video packets, unchanged; a viewer gets a session's packets only
// after its hello.
export type ScreenMessage =
	| { t: "hello"; width: number; height: number }
	| { t: "status"; state: "starting" | "restarting" }
	| { t: "stats"; fps: number; kbps: number };

export type ScreenSessionInfo = {
	serial: string;
	state: "starting" | "running" | "idle";
	width: number | null;
	height: number | null;
	viewers: number;
	fps: number;
	kbps: number;
	startedAt: string | null;
};

// WebSocket close codes the browser acts on.
export const CloseCode = {
	phoneOffline: 4001,
	sessionFailed: 4002,
	shuttingDown: 1001,
} as const;

// Why a viewer could not attach, with the close code to send it.
export class AttachError extends Error {
	constructor(
		readonly code: number,
		message: string,
	) {
		super(message);
	}
}

// A session that ran this long resets the restart budget.
const HEALTHY_AFTER_MS = 10_000;

type Entry = {
	serial: string;
	viewers: Set<Viewer>;
	session: ScrcpySession | null;
	// Set while reconcile() is running; `again` asks it to loop once more.
	busy: boolean;
	again: boolean;
	idleTimer: Timer | undefined;
	idle: boolean;
	retryTimer: Timer | undefined;
	failures: number;
	window: { packets: number; bytes: number; since: number };
	rate: { fps: number; kbps: number };
};

export type ViewerHandle = {
	// Validates and forwards one control message; false when it was refused.
	control(message: Uint8Array): boolean;
	detach(): void;
};

// Owns every screen session: at most one scrcpy-server per phone, shared by
// all of that phone's viewers.
export class ScreenManager {
	private readonly entries = new Map<string, Entry>();
	private readonly idleMs: number;
	private readonly restartDelaysMs: number[];
	private readonly statsTimer: Timer;
	private readonly unsubscribe: () => void;
	private stopping = false;

	constructor(private readonly options: ScreenManagerOptions) {
		this.idleMs = options.idleMs ?? 30_000;
		this.restartDelaysMs = options.restartDelaysMs ?? [500, 2_000, 5_000];
		this.statsTimer = setInterval(
			() => this.publishStats(),
			options.statsIntervalMs ?? 2_000,
		);
		this.unsubscribe = options.devices.on("disconnected", (serial) =>
			this.phoneGone(serial),
		);
	}

	// Adds a viewer to a phone's session, starting or restarting it as needed.
	// Throws an AttachError when the phone is not online or the server is
	// shutting down.
	attach(serial: string, sink: ViewerSink, login: string | null): ViewerHandle {
		if (this.stopping)
			throw new AttachError(
				CloseCode.shuttingDown,
				"the server is shutting down",
			);
		if (this.options.devices.get(serial)?.status !== "online")
			throw new AttachError(
				CloseCode.phoneOffline,
				`phone ${serial} is not online`,
			);
		let entry = this.entries.get(serial);
		if (!entry) {
			entry = {
				serial,
				viewers: new Set(),
				session: null,
				busy: false,
				again: false,
				idleTimer: undefined,
				idle: false,
				retryTimer: undefined,
				failures: 0,
				window: { packets: 0, bytes: 0, since: Date.now() },
				rate: { fps: 0, kbps: 0 },
			};
			this.entries.set(serial, entry);
		}
		const viewer = new Viewer(sink, login, Date.now());
		viewer.auditId = this.openAudit(serial, viewer);
		entry.viewers.add(viewer);
		clearTimeout(entry.idleTimer);
		entry.idleTimer = undefined;
		entry.idle = false;

		const session = entry.session;
		if (session?.running) {
			this.greet(viewer, session);
			session.requestKeyframe();
			// The phone may have gone to sleep since the session started; wake()
			// asks for another keyframe once it is awake.
			void this.wake(session);
		} else viewer.sendJson({ t: "status", state: "starting" });
		this.reconcile(entry);

		const owner = entry;
		return {
			control: (message) => this.control(owner, viewer, message),
			detach: () => this.detach(owner, viewer),
		};
	}

	list(): ScreenSessionInfo[] {
		return [...this.entries.values()].map((entry) => {
			const session = entry.session;
			return {
				serial: entry.serial,
				state: session?.running ? "running" : session ? "starting" : "idle",
				width: session?.meta?.width ?? null,
				height: session?.meta?.height ?? null,
				viewers: entry.viewers.size,
				fps: entry.rate.fps,
				kbps: entry.rate.kbps,
				startedAt: session?.running
					? new Date(session.startedAt).toISOString()
					: null,
			};
		});
	}

	// Closes every viewer and stops every session, resolving once each
	// server has been killed.
	async stop(): Promise<void> {
		this.stopping = true;
		clearInterval(this.statsTimer);
		this.unsubscribe();
		const sessions: Promise<void>[] = [];
		for (const entry of this.entries.values()) {
			clearTimeout(entry.idleTimer);
			clearTimeout(entry.retryTimer);
			// Taken before the viewers leave, because their detach lets
			// reconcile() drop the session without anyone awaiting its stop.
			if (entry.session) sessions.push(entry.session.stop());
			entry.session = null;
			for (const viewer of [...entry.viewers]) {
				this.detach(entry, viewer);
				viewer.sink.close(CloseCode.shuttingDown, "server shutting down");
			}
		}
		this.entries.clear();
		await Promise.all(sessions);
	}

	// Brings the phone's session in line with its viewers: one while anyone
	// watches, kept until the idle timer fires, then none. Runs one step at a
	// time per phone.
	private reconcile(entry: Entry): void {
		if (entry.busy) {
			entry.again = true;
			return;
		}
		entry.busy = true;
		void (async () => {
			try {
				do {
					entry.again = false;
					await this.step(entry);
				} while (entry.again);
			} finally {
				entry.busy = false;
				if (
					!entry.session &&
					entry.viewers.size === 0 &&
					this.entries.get(entry.serial) === entry
				) {
					clearTimeout(entry.retryTimer);
					this.entries.delete(entry.serial);
				}
			}
		})();
	}

	private async step(entry: Entry): Promise<void> {
		const keep = !this.stopping && !(entry.viewers.size === 0 && entry.idle);
		const current = entry.session;
		if (current && !keep) {
			entry.session = null;
			await current.stop();
		}
		if (!keep || entry.viewers.size === 0 || entry.session || entry.retryTimer)
			return;

		let watchdog: Timer | undefined;
		const session: ScrcpySession = new ScrcpySession(
			this.options.devices,
			entry.serial,
			this.options.jar,
			{
				packet: (packet) => this.broadcast(entry, session, packet),
				ended: (error) => {
					clearInterval(watchdog);
					this.ended(entry, session, error);
				},
			},
			this.options.log,
			this.options.session,
		);
		entry.session = session;
		try {
			await session.start();
		} catch {
			// ended() has scheduled the retry or given up.
			return;
		}
		if (entry.session !== session) return;
		// Viewers get packets only after their hello; wake() then asks for a
		// fresh config packet and keyframe for them to start from.
		for (const viewer of entry.viewers) this.greet(viewer, session);
		await this.wake(session);
		if (session.running) watchdog = this.keepAwake(entry, session);
	}

	// While the session has viewers, checks every wakeCheckMs that the phone
	// is still awake, and wakes it if not: a phone that goes to sleep
	// mid-session (a power-button press, measured on the B30) stops sending
	// frames to everyone watching. Nobody watching during the idle window
	// means no check, so an unwatched phone may sleep. One check at a time;
	// the session's end stops it.
	private keepAwake(entry: Entry, session: ScrcpySession): Timer {
		let checking = false;
		// Waking can light the panel after the off we sent, so the next tick
		// darkens it once more.
		let darkenAgain = false;
		// Consecutive checks that found the phone asleep.
		let asleep = 0;
		const timer = setInterval(async () => {
			if (!session.running) {
				clearInterval(timer);
				return;
			}
			if (checking || entry.viewers.size === 0) return;
			checking = true;
			try {
				if (darkenAgain) {
					darkenAgain = false;
					await session.write(setDisplayPower(false));
				}
				const state = parseWakefulness(
					new TextDecoder().decode(
						await this.options.devices.exec(
							session.serial,
							WAKEFULNESS_COMMAND,
							{ timeoutMs: 5_000 },
						),
					),
				);
				if (!state || !session.running) return;
				if (state === "Awake") {
					asleep = 0;
					return;
				}
				asleep++;
				if (asleep === 1)
					this.options.log("screen-woke", {
						serial: session.serial,
						was: state,
					});
				else if (asleep % 6 === 0)
					this.options.log("screen-wake-not-sticking", {
						serial: session.serial,
						state,
						checks: asleep,
					});
				await this.wake(session);
				darkenAgain = true;
			} catch {
				// The phone is gone or busy; the next check tries again.
			} finally {
				checking = false;
			}
		}, this.options.wakeCheckMs ?? 5_000);
		return timer;
	}

	// Wakes the phone and dismisses its keyguard, so the screen shows
	// something that changes, then asks for a fresh keyframe and darkens the
	// phone's own display again: waking turns it on, and it stays dark while
	// the phone is watched.
	private async wake(session: ScrcpySession): Promise<void> {
		try {
			await this.options.devices.exec(session.serial, WAKE_COMMAND, {
				timeoutMs: 10_000,
			});
		} catch (error) {
			this.options.log("screen-wake-failed", {
				serial: session.serial,
				error: error instanceof Error ? error.message : String(error),
			});
		}
		if (!session.running) return;
		session.requestKeyframe();
		await session.write(setDisplayPower(false));
	}

	private greet(viewer: Viewer, session: ScrcpySession): void {
		const meta = session.meta;
		if (!meta) return;
		viewer.sendJson({
			t: "hello",
			width: meta.width,
			height: meta.height,
		} satisfies ScreenMessage);
		viewer.greetedScid = session.scid;
		viewer.delivery.needsConfig = true;
		viewer.delivery.waitingForKeyframe = true;
		viewer.delivery.keyframeRequestedAt = Date.now();
	}

	private broadcast(
		entry: Entry,
		session: ScrcpySession,
		packet: VideoPacket,
	): void {
		if (entry.session !== session || !session.running) return;
		entry.window.packets += packet.config ? 0 : 1;
		entry.window.bytes += packet.bytes.length;
		const now = Date.now();
		let wantsKeyframe = false;
		for (const viewer of entry.viewers)
			if (
				viewer.greetedScid === session.scid &&
				viewer.deliver(packet, BACKLOG_LIMIT, now)
			)
				wantsKeyframe = true;
		if (wantsKeyframe) session.requestKeyframe();
	}

	private ended(
		entry: Entry,
		session: ScrcpySession,
		error: Error | null,
	): void {
		if (entry.session !== session) return;
		entry.session = null;
		if (error === null || entry.viewers.size === 0 || this.stopping) {
			this.reconcile(entry);
			return;
		}
		const ranFor = session.startedAt ? Date.now() - session.startedAt : 0;
		if (ranFor >= HEALTHY_AFTER_MS) entry.failures = 0;
		const delay = this.restartDelaysMs[entry.failures++];
		if (delay === undefined) {
			entry.failures = 0;
			for (const viewer of [...entry.viewers]) {
				this.detach(entry, viewer);
				viewer.sink.close(
					CloseCode.sessionFailed,
					`screen session failed: ${error.message}`.slice(0, 120),
				);
			}
			return;
		}
		for (const viewer of entry.viewers)
			viewer.sendJson({ t: "status", state: "restarting" });
		clearTimeout(entry.retryTimer);
		entry.retryTimer = setTimeout(() => {
			entry.retryTimer = undefined;
			this.reconcile(entry);
		}, delay);
	}

	private detach(entry: Entry, viewer: Viewer): void {
		if (!entry.viewers.delete(viewer)) return;
		this.closeAudit(viewer);
		if (entry.viewers.size === 0) {
			clearTimeout(entry.idleTimer);
			entry.idleTimer = setTimeout(() => {
				entry.idleTimer = undefined;
				entry.idle = true;
				this.reconcile(entry);
			}, this.idleMs);
		}
		this.reconcile(entry);
	}

	private control(entry: Entry, viewer: Viewer, message: Uint8Array): boolean {
		const kind = classifyControl(message);
		const session = entry.session;
		if (!kind || !session?.running) {
			viewer.counts.rejected++;
			return false;
		}
		if (kind === "text") viewer.counts.texts++;
		else viewer.counts.inputs++;
		void session.write(message);
		return true;
	}

	private phoneGone(serial: string): void {
		const entry = this.entries.get(serial);
		if (!entry) return;
		clearTimeout(entry.retryTimer);
		entry.retryTimer = undefined;
		for (const viewer of [...entry.viewers]) {
			this.detach(entry, viewer);
			viewer.sink.close(CloseCode.phoneOffline, "phone offline");
		}
		entry.idle = true;
		this.reconcile(entry);
	}

	private publishStats(): void {
		const now = Date.now();
		for (const entry of this.entries.values()) {
			const seconds = Math.max(0.001, (now - entry.window.since) / 1000);
			entry.rate = {
				fps: Math.round((entry.window.packets / seconds) * 10) / 10,
				kbps: Math.round((entry.window.bytes * 8) / 1000 / seconds),
			};
			entry.window = { packets: 0, bytes: 0, since: now };
			for (const viewer of entry.viewers)
				viewer.sendJson({ t: "stats", ...entry.rate } satisfies ScreenMessage);
		}
	}

	// One control_sessions row per viewer: who, which phone, when, and
	// how many inputs. Typed text is counted, never stored.
	private openAudit(serial: string, viewer: Viewer): number | null {
		const phone = this.options.db
			.query<{ id: number }, [string]>("SELECT id FROM phones WHERE serial = ?")
			.get(serial);
		if (!phone) return null;
		return (
			this.options.db
				.query<{ id: number }, [number, string | null, string]>(
					"INSERT INTO control_sessions (phone_id, login, started_at) VALUES (?, ?, ?) RETURNING id",
				)
				.get(phone.id, viewer.login, new Date().toISOString())?.id ?? null
		);
	}

	private closeAudit(viewer: Viewer): void {
		if (viewer.auditId === null) return;
		const { inputs, texts, rejected } = viewer.counts;
		this.options.db
			.query(
				"UPDATE control_sessions SET ended_at = ?, inputs = ?, texts = ?, rejected = ? WHERE id = ?",
			)
			.run(new Date().toISOString(), inputs, texts, rejected, viewer.auditId);
	}
}
