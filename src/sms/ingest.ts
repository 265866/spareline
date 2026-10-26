import type { Database } from "bun:sqlite";
import type { Bus } from "../bus";
import type { Devices } from "../devices/manager";
import type { Log } from "../log";
import { saveAttachment } from "./attachments";
import {
	activitySignature,
	type ContentRow,
	ISMS_TRIGGER_SCRIPT,
	parseIsms,
	toInteger,
} from "./parse";
import {
	checkContentOutput,
	commands,
	inboxKeys,
	MMS_ADDR_COLUMNS,
	MMS_COLUMNS,
	MMS_PART_COLUMNS,
	type MmsPartRow,
	mmsMessage,
	mmsPart,
	type PartRef,
	type Provider,
	type ProviderMessage,
	SMS_COLUMNS,
	smsMessage,
} from "./provider";
import {
	attributePending,
	getCursor,
	insertMessages,
	type Message,
	type MessageStore,
	type NewMessage,
	type StoredAttachment,
	setCursor,
	storedKeys,
} from "./store";

export type SmsIngestOptions = {
	devices: Devices;
	db: Database;
	store: MessageStore;
	// Receives every new message after its transaction commits
	// ("message.ingested"); the extension point for forwarding.
	messages: Bus<Message>;
	attachmentsDir: string;
	log: Log;
	// How often each online phone's `dumpsys isms` is compared.
	triggerMs?: number;
	// How often the whole inbox is compared with the database.
	reconcileMs?: number;
	now?: () => Date;
};

type PhoneLoop = {
	serial: string;
	timer: Timer;
	running: Promise<void> | null;
	signature: string | null;
	followUps: number;
	reconcileDue: number;
	// MMS by provider _id, with what the last read showed. One is stored
	// only when a read shows the same as the one before. `unconfirmed` waits
	// for that second read, which the next tick makes; `confirmed` has agreed
	// but has a part that could not be read yet, retried only on reconcile.
	unconfirmed: Map<number, string>;
	confirmed: Map<number, string>;
	stopped: boolean;
};

// Provider queries after an isms change that found nothing yet: the Messages
// app writes the row shortly after the platform delivers it.
const FOLLOW_UPS = 3;
// Reconciles after which an MMS whose part cannot be read is stored without
// that part.
const PART_ATTEMPTS = 5;
const QUERY_TIMEOUT_MS = 30_000;
const BATCH = 100;

// Copies inbound texts from every online phone's providers into SQLite. The
// provider is the source of truth: `dumpsys isms` (about 45 ms) says when to
// look, a provider query (about 1.1 s) says what arrived, and a periodic
// reconcile catches anything the trigger missed (MMS, cleared logs, a phone
// that was unplugged). Every write goes through insertMessages, whose UNIQUE
// key makes repeats harmless.
export class SmsIngest {
	private readonly loops = new Map<string, PhoneLoop>();
	private readonly unsubscribe: (() => void)[] = [];
	// Failed reconcile reads per "<serial>:<mms id>:<part id>".
	private readonly partFailures = new Map<string, number>();
	private readonly triggerMs: number;
	private readonly reconcileMs: number;
	private readonly now: () => Date;

	constructor(private readonly options: SmsIngestOptions) {
		this.triggerMs = options.triggerMs ?? 3_000;
		this.reconcileMs = options.reconcileMs ?? 60_000;
		this.now = options.now ?? (() => new Date());
	}

	start(): void {
		const { devices } = this.options;
		this.unsubscribe.push(
			// Not "connected": until the identity probe has placed the phone's
			// SIMs, texts on a SIM that just moved here would get no number.
			devices.on("ready", (serial) => this.watch(serial)),
			devices.on("disconnected", (serial) => this.unwatch(serial)),
			devices.on("subsChanged", () => this.attributePending()),
		);
		this.attributePending();
		for (const phone of devices.list())
			if (phone.status === "online" && phone.identityAt !== null)
				this.watch(phone.serial);
	}

	async stop(): Promise<void> {
		for (const off of this.unsubscribe.splice(0)) off();
		const running = [...this.loops.values()].map((loop) => loop.running);
		for (const serial of [...this.loops.keys()]) this.unwatch(serial);
		await Promise.allSettled(running);
	}

	// Waits for the cycles in flight; for tests.
	async settled(): Promise<void> {
		while ([...this.loops.values()].some((loop) => loop.running))
			await Promise.allSettled(
				[...this.loops.values()].map((loop) => loop.running),
			);
	}

	// Runs one cycle for a phone now (trigger check, or reconcile when due).
	tick(serial: string): Promise<void> {
		const loop = this.loops.get(serial);
		if (!loop || loop.stopped) return Promise.resolve();
		if (loop.running) return loop.running;
		loop.running = this.cycle(loop)
			.catch((error: unknown) => {
				if (!loop.stopped)
					this.options.log("sms-ingest-failed", {
						serial,
						error: errorText(error),
					});
			})
			.finally(() => {
				loop.running = null;
			});
		return loop.running;
	}

	// Compares the whole inbox of both providers with the database and stores
	// what is missing. Returns the number of new messages.
	async reconcile(serial: string): Promise<number> {
		const loop = this.loops.get(serial) ?? null;
		let added = 0;
		for (const provider of ["sms", "mms"] as const)
			added += await this.reconcileProvider(serial, provider, loop);
		return added;
	}

	// Gives messages stored without a number their number, at a new seq, and
	// publishes them again so cursors for that number see them.
	attributePending(): void {
		const seqs = attributePending(this.options.db);
		if (seqs.length === 0) return;
		this.options.log("sms-attributed", { seqs });
		this.publish(seqs);
	}

	private watch(serial: string): void {
		if (this.loops.has(serial)) return;
		const loop: PhoneLoop = {
			serial,
			timer: setInterval(() => void this.tick(serial), this.triggerMs),
			running: null,
			signature: null,
			followUps: 0,
			// A phone that (re)connects may hold texts that arrived while it was
			// unplugged or before the app first saw it.
			reconcileDue: 0,
			unconfirmed: new Map(),
			confirmed: new Map(),
			stopped: false,
		};
		this.loops.set(serial, loop);
		void this.tick(serial);
	}

	private unwatch(serial: string): void {
		const loop = this.loops.get(serial);
		if (!loop) return;
		loop.stopped = true;
		clearInterval(loop.timer);
		this.loops.delete(serial);
	}

	private async cycle(loop: PhoneLoop): Promise<void> {
		const now = this.now().getTime();
		if (now >= loop.reconcileDue) {
			// A failed reconcile is retried soon, but not on every tick: each
			// try starts app_process on the phone a few times.
			loop.reconcileDue = now + Math.min(this.reconcileMs, 15_000);
			// The signature is read first so that a text landing during the
			// reconcile still changes it afterwards.
			loop.signature = await this.signature(loop.serial);
			await this.reconcile(loop.serial);
			loop.reconcileDue = this.now().getTime() + this.reconcileMs;
			return;
		}
		const signature = await this.signature(loop.serial);
		if (loop.signature !== null && signature !== loop.signature)
			loop.followUps = FOLLOW_UPS;
		loop.signature = signature;
		if (loop.unconfirmed.size > 0) await this.confirm(loop);
		if (loop.followUps === 0) return;
		loop.followUps--;
		const added =
			(await this.pullNew(loop, "sms")) || (await this.pullNew(loop, "mms"));
		if (added > 0) loop.followUps = 0;
	}

	private async signature(serial: string): Promise<string> {
		const result = await this.options.devices.shell(
			serial,
			ISMS_TRIGGER_SCRIPT,
			{ timeoutMs: 10_000 },
		);
		// grep exits 1 when nothing matches, which is a valid (empty) answer.
		if (result.exitCode > 1)
			throw new Error(`dumpsys isms exited ${result.exitCode}`);
		return activitySignature(parseIsms(text(result.stdout)));
	}

	// Stores provider rows after the cursor. Returns the number of new messages.
	private async pullNew(loop: PhoneLoop, provider: Provider): Promise<number> {
		const phoneId = this.phoneId(loop.serial);
		if (phoneId === null) return 0;
		const cursor = getCursor(this.options.db, phoneId, provider);
		const rows = await this.queryRows(
			loop.serial,
			provider === "sms"
				? commands.smsAfter(cursor)
				: commands.mmsAfter(cursor),
			provider === "sms" ? SMS_COLUMNS : MMS_COLUMNS,
		);
		if (rows.length === 0) return 0;
		const { messages, complete } = await this.build(
			loop.serial,
			provider,
			rows,
			loop,
			false,
		);
		const added = this.store(loop.serial, phoneId, messages);
		const next = Math.max(cursor, ...complete);
		if (next > cursor) setCursor(this.options.db, phoneId, provider, next);
		return added;
	}

	// Reads the MMS seen once again, and stores the ones that read the same.
	private async confirm(loop: PhoneLoop): Promise<void> {
		const phoneId = this.phoneId(loop.serial);
		if (phoneId === null) return;
		const ids = [...loop.unconfirmed.keys()];
		const rows = await this.queryRows(
			loop.serial,
			commands.mmsByIds(ids),
			MMS_COLUMNS,
		);
		const present = new Set(rows.map((row) => toInteger(row._id)));
		// Gone (a notification replaced by its download): nothing to confirm.
		for (const id of ids) if (!present.has(id)) loop.unconfirmed.delete(id);
		const { messages } = await this.build(
			loop.serial,
			"mms",
			rows,
			loop,
			false,
		);
		this.store(loop.serial, phoneId, messages);
	}

	private async reconcileProvider(
		serial: string,
		provider: Provider,
		loop: PhoneLoop | null,
	): Promise<number> {
		const phoneId = this.phoneId(serial);
		if (phoneId === null) return 0;
		const keys = inboxKeys(
			await this.queryRows(
				serial,
				provider === "sms" ? commands.smsKeys() : commands.mmsKeys(),
				["_id", "date"],
			),
			provider,
		);
		const stored = storedKeys(this.options.db, phoneId, provider);
		const missing = keys
			.filter((key) => !stored.has(`${key.providerId}|${key.receivedAt}`))
			.map((key) => key.providerId);
		let added = 0;
		for (let i = 0; i < missing.length; i += BATCH) {
			const ids = missing.slice(i, i + BATCH);
			const rows = await this.queryRows(
				serial,
				provider === "sms" ? commands.smsByIds(ids) : commands.mmsByIds(ids),
				provider === "sms" ? SMS_COLUMNS : MMS_COLUMNS,
			);
			const { messages } = await this.build(serial, provider, rows, loop, true);
			added += this.store(serial, phoneId, messages);
		}
		// The provider's highest _id, even when it is lower than the cursor:
		// Messages' data was cleared and ids start over.
		const highest = Math.max(0, ...keys.map((key) => key.providerId));
		setCursor(this.options.db, phoneId, provider, highest);
		return added;
	}

	private async queryRows(
		serial: string,
		command: string,
		columns: readonly string[],
	): Promise<ContentRow[]> {
		const result = await this.options.devices.shell(serial, command, {
			timeoutMs: QUERY_TIMEOUT_MS,
		});
		const parsed = checkContentOutput(
			{
				stdout: text(result.stdout),
				stderr: text(result.stderr),
				exitCode: result.exitCode,
			},
			columns,
		);
		if (parsed.malformed.length > 0)
			this.options.log("sms-rows-malformed", {
				serial,
				rows: parsed.malformed.length,
				// Only the row id: senders and bodies stay out of the log.
				sample: /^Row: \d+ _id=\d+/.exec(parsed.malformed[0] ?? "")?.[0],
			});
		return parsed.rows;
	}

	// Turns provider rows into messages ready to store, reading MMS addresses
	// and parts. `complete` lists the row ids that need no more reading, in
	// order, up to the first that does (an MMS not confirmed yet, or a part
	// not readable yet); the cursor may move that far.
	private async build(
		serial: string,
		provider: Provider,
		rows: ContentRow[],
		loop: PhoneLoop | null,
		reconciling: boolean,
	): Promise<{ messages: NewMessage[]; complete: number[] }> {
		const parsed: ProviderMessage[] = [];
		// Rows that can never become a message; they do not hold the cursor.
		const invalid = new Set<number>();
		// MMS rows already stored; the cursor may pass them.
		const done = new Set<number>();
		if (provider === "sms") {
			for (const row of rows) {
				try {
					parsed.push(smsMessage(row));
				} catch (error) {
					const id = toInteger(row._id);
					if (id !== null) invalid.add(id);
					this.options.log("sms-row-invalid", {
						serial,
						id: row._id,
						error: errorText(error),
					});
				}
			}
		} else {
			parsed.push(
				...(await this.readMms(serial, rows, loop, invalid, done, reconciling)),
			);
		}
		const ready = new Map<number, NewMessage>();
		for (const message of parsed) {
			const attachments = await this.readParts(serial, message, reconciling);
			if (attachments === null) continue;
			if (message.provider === "mms")
				loop?.confirmed.delete(message.providerId);
			const { parts: _parts, ...rest } = message;
			ready.set(message.providerId, { ...rest, attachments });
		}
		const complete: number[] = [];
		for (const row of rows) {
			const id = toInteger(row._id);
			if (id === null || invalid.has(id)) continue;
			if (!ready.has(id) && !done.has(id)) break;
			complete.push(id);
		}
		return { messages: [...ready.values()], complete };
	}

	// MMS rows with their addresses and parts. AOSP's download code inserts
	// the row and then updates its date and subscription, so a read can catch
	// a half-written MMS; each one is returned only once two reads in a row
	// agree on its date, sub id, and parts. Without a loop (a one-off
	// reconcile) there is no earlier read, so nothing is returned.
	private async readMms(
		serial: string,
		rows: ContentRow[],
		loop: PhoneLoop | null,
		invalid: Set<number>,
		done: Set<number>,
		reconciling: boolean,
	): Promise<ProviderMessage[]> {
		const phoneId = this.phoneId(serial);
		const stored =
			phoneId === null
				? new Set<string>()
				: storedKeys(this.options.db, phoneId, "mms");
		const valid = rows.flatMap((row) => {
			const id = toInteger(row._id);
			if (id === null) {
				this.options.log("mms-row-invalid", {
					serial,
					error: "no integer _id",
				});
				return [];
			}
			const key = inboxKeys([row], "mms")[0];
			if (key && stored.has(`${key.providerId}|${key.receivedAt}`)) {
				done.add(id);
				loop?.unconfirmed.delete(id);
				loop?.confirmed.delete(id);
				return [];
			}
			return [{ id, row }];
		});
		if (valid.length === 0) return [];
		const parts = (
			await this.queryRows(
				serial,
				commands.mmsParts(valid.map(({ id }) => id)),
				MMS_PART_COLUMNS,
			)
		).flatMap((row) => {
			try {
				return [mmsPart(row)];
			} catch {
				return [];
			}
		});
		const messages: ProviderMessage[] = [];
		for (const { id, row } of valid) {
			const own = parts.filter((part) => part.mid === id);
			const seen = mmsSignature(row, own);
			if (!loop) continue;
			if (loop.confirmed.get(id) === seen) {
				// Agreed earlier; its unreadable part waits for the next reconcile.
				if (!reconciling) continue;
			} else if (loop.unconfirmed.get(id) === seen) {
				loop.unconfirmed.delete(id);
				loop.confirmed.set(id, seen);
			} else {
				loop.confirmed.delete(id);
				loop.unconfirmed.set(id, seen);
				continue;
			}
			const addresses = await this.queryRows(
				serial,
				commands.mmsAddr(id),
				MMS_ADDR_COLUMNS,
			);
			try {
				messages.push(mmsMessage(row, addresses, own));
			} catch (error) {
				invalid.add(id);
				this.options.log("mms-row-invalid", {
					serial,
					id,
					error: errorText(error),
				});
			}
		}
		return messages;
	}

	// Streams each binary part to a file. Returns null when a part cannot be
	// read yet and the message should be retried. Only reconciles count
	// toward giving up, so the follow-up reads after a trigger do not burn
	// the attempts.
	private async readParts(
		serial: string,
		message: ProviderMessage,
		reconciling: boolean,
	): Promise<StoredAttachment[] | null> {
		const stored: StoredAttachment[] = [];
		for (const part of message.parts) {
			const key = `${serial}:${message.providerId}:${part.partId}`;
			const read = await this.readPart(serial, part);
			if (read.ok) {
				this.partFailures.delete(key);
				stored.push({
					contentType: part.contentType,
					filename: part.filename,
					...saveAttachment(this.options.attachmentsDir, read.bytes),
				});
				continue;
			}
			const failures =
				(this.partFailures.get(key) ?? 0) + (reconciling ? 1 : 0);
			this.options.log("mms-part-unreadable", {
				serial,
				mms: message.providerId,
				part: part.partId,
				attempt: failures,
				reason: read.reason,
			});
			if (failures < PART_ATTEMPTS) {
				this.partFailures.set(key, failures);
				return null;
			}
			this.partFailures.delete(key);
		}
		return stored;
	}

	// `content read` over the shell protocol: binary stdout, stderr apart, and
	// an exit status. The tool prints provider errors on stderr and still
	// exits 0, so any stderr counts as a failure.
	private async readPart(
		serial: string,
		part: PartRef,
	): Promise<{ ok: true; bytes: Uint8Array } | { ok: false; reason: string }> {
		try {
			const result = await this.options.devices.shell(
				serial,
				commands.readPart(part.partId),
				{ timeoutMs: 120_000 },
			);
			if (result.exitCode !== 0)
				return { ok: false, reason: `exit ${result.exitCode}` };
			if (result.stderr.length > 0)
				return {
					ok: false,
					reason: `stderr: ${text(result.stderr).split("\n")[0]?.slice(0, 120)}`,
				};
			if (result.stdout.length === 0) return { ok: false, reason: "empty" };
			return { ok: true, bytes: result.stdout };
		} catch (error) {
			// Includes a shell that ended without its exit packet.
			return { ok: false, reason: errorText(error) };
		}
	}

	private store(
		serial: string,
		phoneId: number,
		messages: NewMessage[],
	): number {
		if (messages.length === 0) return 0;
		const seqs = insertMessages(
			this.options.db,
			phoneId,
			messages,
			this.now().toISOString(),
		);
		if (seqs.length === 0) return 0;
		this.options.log("sms-ingested", { serial, seqs });
		this.publish(seqs);
		return seqs.length;
	}

	private publish(seqs: number[]): void {
		for (const seq of seqs) {
			const message = this.options.store.get(seq);
			if (message) this.options.messages.publish(message);
		}
	}

	private phoneId(serial: string): number | null {
		return (
			this.options.db
				.query<{ id: number }, [string]>(
					"SELECT id FROM phones WHERE serial = ?",
				)
				.get(serial)?.id ?? null
		);
	}
}

function mmsSignature(row: ContentRow, parts: MmsPartRow[]): string {
	return JSON.stringify([
		row.date,
		row.sub_id,
		row.m_type,
		parts.map((part) => [part.partId, part.contentType, part.text.length]),
	]);
}

function text(bytes: Uint8Array): string {
	return new TextDecoder().decode(bytes);
}

function errorText(error: unknown): string {
	return error instanceof Error ? error.message : String(error);
}
