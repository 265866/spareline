import type { Database } from "bun:sqlite";
import type { DeviceEventRow } from "./sync";

export type PhoneRow = {
	id: number;
	serial: string;
	site: string;
	manufacturer: string | null;
	model: string | null;
	androidVersion: string | null;
	sdk: number | null;
	imeis: string[];
	location: string | null;
	hubPort: string | null;
	costCents: number | null;
	acquiredOn: string | null;
	lastState: string;
	firstSeenAt: string;
	lastOnlineAt: string | null;
	// SIMs currently placed in this phone, by slot.
	placements: PlacementRow[];
};

type PlacementRow = {
	slot: number;
	subId: number;
	since: string;
	sim: { id: number; iccid: string; carrier: string | null };
	number: NumberSummary | null;
};

export type NumberSummary = {
	id: number;
	e164: string | null;
	source: "detected" | "manual" | "none";
	note: string;
};

type SimRow = {
	id: number;
	iccid: string;
	carrier: string | null;
	status: "active" | "spare" | "retired";
	firstSeenAt: string;
	lastSeenAt: string;
	placement: {
		phoneId: number;
		serial: string;
		slot: number;
		subId: number;
		since: string;
	} | null;
	lastPlacement: { serial: string; slot: number; until: string } | null;
	number: NumberSummary | null;
};

export type NumberRow = NumberSummary & {
	detectedE164: string | null;
	confirmedBy: string | null;
	createdAt: string;
	sim: { id: number; iccid: string; carrier: string | null } | null;
	// Where the SIM is now, or where it was last seen when it is out.
	phone: {
		serial: string;
		model: string | null;
		slot: number;
		removedAt: string | null;
	} | null;
};

export class ConflictError extends Error {}
export class NotFoundError extends Error {}

type PhoneDb = {
	id: number;
	serial: string;
	site: string;
	manufacturer: string | null;
	model: string | null;
	android_version: string | null;
	sdk: number | null;
	imei1: string | null;
	imei2: string | null;
	location: string | null;
	hub_port: string | null;
	cost_cents: number | null;
	acquired_on: string | null;
	last_state: string;
	first_seen_at: string;
	last_online_at: string | null;
};

export class Inventory {
	constructor(private readonly db: Database) {}

	listPhones(): PhoneRow[] {
		const rows = this.db
			.query<PhoneDb, []>("SELECT * FROM phones ORDER BY serial")
			.all();
		return rows.map((row) => this.phoneRow(row));
	}

	getPhone(serial: string): PhoneRow | null {
		const row = this.db
			.query<PhoneDb, [string]>("SELECT * FROM phones WHERE serial = ?")
			.get(serial);
		return row ? this.phoneRow(row) : null;
	}

	patchPhone(
		serial: string,
		patch: {
			location?: string | null;
			hubPort?: string | null;
			costCents?: number | null;
			acquiredOn?: string | null;
		},
		login: string | null,
	): PhoneRow {
		const phone = this.getPhone(serial);
		if (!phone) throw new NotFoundError(`no phone ${serial}`);
		this.db.transaction(() => {
			const columns: [string, unknown][] = [];
			if (patch.location !== undefined)
				columns.push(["location", patch.location]);
			if (patch.hubPort !== undefined)
				columns.push(["hub_port", patch.hubPort]);
			if (patch.costCents !== undefined)
				columns.push(["cost_cents", patch.costCents]);
			if (patch.acquiredOn !== undefined)
				columns.push(["acquired_on", patch.acquiredOn]);
			this.update("phones", phone.id, columns);
			this.audit(login, "phone", phone.id, patch);
		})();
		return this.getPhone(serial) as PhoneRow;
	}

	listSims(): SimRow[] {
		return this.db
			.query<{ id: number }, []>("SELECT id FROM sims ORDER BY id")
			.all()
			.map((row) => this.getSim(row.id) as SimRow);
	}

	getSim(id: number): SimRow | null {
		const sim = this.db
			.query<
				{
					id: number;
					iccid: string;
					carrier: string | null;
					status: SimRow["status"];
					first_seen_at: string;
					last_seen_at: string;
				},
				[number]
			>(
				"SELECT id, iccid, carrier, status, first_seen_at, last_seen_at FROM sims WHERE id = ?",
			)
			.get(id);
		if (!sim) return null;
		const placement = this.db
			.query<
				{
					phone_id: number;
					serial: string;
					slot: number;
					sub_id: number;
					from_at: string;
				},
				[number]
			>(
				`SELECT p.phone_id, ph.serial, p.slot, p.sub_id, p.from_at FROM sim_placements p JOIN phones ph ON ph.id = p.phone_id
				WHERE p.sim_id = ? AND p.to_at IS NULL`,
			)
			.get(id);
		const last = placement
			? null
			: this.db
					.query<{ serial: string; slot: number; to_at: string }, [number]>(
						`SELECT ph.serial, p.slot, p.to_at FROM sim_placements p JOIN phones ph ON ph.id = p.phone_id
						WHERE p.sim_id = ? ORDER BY p.to_at DESC LIMIT 1`,
					)
					.get(id);
		return {
			id: sim.id,
			iccid: sim.iccid,
			carrier: sim.carrier,
			status: sim.status,
			firstSeenAt: sim.first_seen_at,
			lastSeenAt: sim.last_seen_at,
			placement: placement
				? {
						phoneId: placement.phone_id,
						serial: placement.serial,
						slot: placement.slot,
						subId: placement.sub_id,
						since: placement.from_at,
					}
				: null,
			lastPlacement: last
				? { serial: last.serial, slot: last.slot, until: last.to_at }
				: null,
			number: this.numberOfSim(id),
		};
	}

	findSim(key: string): SimRow | null {
		const row = /^\d{1,9}$/.test(key)
			? { id: Number(key) }
			: this.db
					.query<{ id: number }, [string]>(
						"SELECT id FROM sims WHERE iccid = ?",
					)
					.get(key);
		return row ? this.getSim(row.id) : null;
	}

	patchSim(
		id: number,
		patch: { status?: SimRow["status"] },
		login: string | null,
	): SimRow {
		if (!this.getSim(id)) throw new NotFoundError(`no SIM ${id}`);
		this.db.transaction(() => {
			const columns: [string, unknown][] = [];
			if (patch.status !== undefined) columns.push(["status", patch.status]);
			this.update("sims", id, columns);
			this.audit(login, "sim", id, patch);
		})();
		return this.getSim(id) as SimRow;
	}

	listNumbers(): NumberRow[] {
		return this.db
			.query<{ id: number }, []>(
				"SELECT id FROM numbers ORDER BY e164 IS NULL, e164, id",
			)
			.all()
			.map((row) => this.getNumber(row.id) as NumberRow);
	}

	// By id, or by E.164 when the key starts with "+".
	findNumber(key: string): NumberRow | null {
		const id = this.numberIdOf(key);
		return id === null ? null : this.getNumber(id);
	}

	// The id of a number by id, or by E.164 when the key starts with "+".
	numberIdOf(key: string): number | null {
		const row = key.startsWith("+")
			? this.db
					.query<{ id: number }, [string]>(
						"SELECT id FROM numbers WHERE e164 = ?",
					)
					.get(key)
			: /^\d{1,9}$/.test(key)
				? this.db
						.query<{ id: number }, [number]>(
							"SELECT id FROM numbers WHERE id = ?",
						)
						.get(Number(key))
				: null;
		return row?.id ?? null;
	}

	getNumber(id: number): NumberRow | null {
		const number = this.db
			.query<
				{
					id: number;
					e164: string | null;
					e164_source: NumberRow["source"];
					note: string;
					created_at: string;
				},
				[number]
			>(
				"SELECT id, e164, e164_source, note, created_at FROM numbers WHERE id = ?",
			)
			.get(id);
		if (!number) return null;
		// The open binding, or the most recent one when the SIM is gone.
		const binding = this.db
			.query<
				{
					sim_id: number;
					detected_e164: string | null;
					confirmed_by: string | null;
					iccid: string;
					carrier: string | null;
				},
				[number]
			>(
				`SELECT b.sim_id, b.detected_e164, b.confirmed_by, s.iccid, s.carrier FROM number_bindings b JOIN sims s ON s.id = b.sim_id
				WHERE b.number_id = ? ORDER BY b.to_at IS NOT NULL, b.from_at DESC LIMIT 1`,
			)
			.get(id);
		const placement = binding
			? this.db
					.query<
						{
							serial: string;
							model: string | null;
							slot: number;
							to_at: string | null;
						},
						[number]
					>(
						`SELECT ph.serial, ph.model, p.slot, p.to_at FROM sim_placements p JOIN phones ph ON ph.id = p.phone_id
						WHERE p.sim_id = ? ORDER BY p.to_at IS NOT NULL, p.from_at DESC LIMIT 1`,
					)
					.get(binding.sim_id)
			: null;
		return {
			id: number.id,
			e164: number.e164,
			source: number.e164_source,
			note: number.note,
			detectedE164: binding?.detected_e164 ?? null,
			confirmedBy: binding?.confirmed_by ?? null,
			createdAt: number.created_at,
			sim: binding
				? { id: binding.sim_id, iccid: binding.iccid, carrier: binding.carrier }
				: null,
			phone: placement
				? {
						serial: placement.serial,
						model: placement.model,
						slot: placement.slot,
						removedAt: placement.to_at,
					}
				: null,
		};
	}

	// e164: a normalized number to set by hand, null to return to the detected
	// one, undefined to leave as is.
	patchNumber(
		id: number,
		patch: { note?: string; e164?: string | null },
		login: string | null,
	): NumberRow {
		const number = this.getNumber(id);
		if (!number) throw new NotFoundError(`no number ${id}`);
		this.db.transaction(() => {
			if (patch.note !== undefined)
				this.update("numbers", id, [["note", patch.note]]);
			if (patch.e164 !== undefined) {
				const target = patch.e164 ?? number.detectedE164;
				if (target !== null) {
					const owner = this.db
						.query<{ id: number }, [string]>(
							"SELECT id FROM numbers WHERE e164 = ?",
						)
						.get(target);
					if (owner && owner.id !== id)
						throw new ConflictError(
							`${target} already belongs to number ${owner.id}`,
						);
				}
				const source =
					patch.e164 !== null
						? "manual"
						: target === null
							? "none"
							: "detected";
				this.db.run(
					"UPDATE numbers SET e164 = ?, e164_source = ? WHERE id = ?",
					[target, source, id],
				);
				this.db.run(
					"UPDATE number_bindings SET confirmed_by = ? WHERE number_id = ? AND to_at IS NULL",
					[patch.e164 === null ? null : (login ?? "unknown"), id],
				);
			}
			this.audit(login, "number", id, patch);
		})();
		return this.getNumber(id) as NumberRow;
	}

	// Events with id > `after`, oldest first: a forward cursor for paging and
	// for resuming an event stream.
	eventsAfter(after: number, limit: number): DeviceEventRow[] {
		return this.queryEvents(
			"WHERE e.id > ? ORDER BY e.id ASC LIMIT ?",
			after,
			limit,
		);
	}

	// The newest `count` events, oldest first.
	latestEvents(count: number): DeviceEventRow[] {
		return this.queryEvents(
			"WHERE e.id > ? ORDER BY e.id DESC LIMIT ?",
			0,
			count,
		).reverse();
	}

	private queryEvents(
		clause: string,
		after: number,
		limit: number,
	): DeviceEventRow[] {
		const rows = this.db
			.query<
				{
					id: number;
					phone_id: number | null;
					serial: string | null;
					at: string;
					kind: string;
					detail_json: string;
				},
				[number, number]
			>(
				`SELECT e.id, e.phone_id, p.serial, e.at, e.kind, e.detail_json FROM device_events e LEFT JOIN phones p ON p.id = e.phone_id ${clause}`,
			)
			.all(after, limit);
		return rows.map((row) => ({
			id: row.id,
			phoneId: row.phone_id,
			serial: row.serial,
			at: row.at,
			kind: row.kind,
			detail: JSON.parse(row.detail_json) as Record<string, unknown>,
		}));
	}

	private phoneRow(row: PhoneDb): PhoneRow {
		const placements = this.db
			.query<
				{
					slot: number;
					sub_id: number;
					from_at: string;
					sim_id: number;
					iccid: string;
					carrier: string | null;
				},
				[number]
			>(
				`SELECT p.slot, p.sub_id, p.from_at, s.id AS sim_id, s.iccid, s.carrier FROM sim_placements p
				JOIN sims s ON s.id = p.sim_id WHERE p.phone_id = ? AND p.to_at IS NULL ORDER BY p.slot`,
			)
			.all(row.id);
		return {
			id: row.id,
			serial: row.serial,
			site: row.site,
			manufacturer: row.manufacturer,
			model: row.model,
			androidVersion: row.android_version,
			sdk: row.sdk,
			imeis: [row.imei1, row.imei2].filter(
				(imei): imei is string => imei !== null,
			),
			location: row.location,
			hubPort: row.hub_port,
			costCents: row.cost_cents,
			acquiredOn: row.acquired_on,
			lastState: row.last_state,
			firstSeenAt: row.first_seen_at,
			lastOnlineAt: row.last_online_at,
			placements: placements.map((placement) => ({
				slot: placement.slot,
				subId: placement.sub_id,
				since: placement.from_at,
				sim: {
					id: placement.sim_id,
					iccid: placement.iccid,
					carrier: placement.carrier,
				},
				number: this.numberOfSim(placement.sim_id),
			})),
		};
	}

	private numberOfSim(simId: number): NumberSummary | null {
		const row = this.db
			.query<
				{
					id: number;
					e164: string | null;
					e164_source: NumberSummary["source"];
					note: string;
				},
				[number]
			>(
				`SELECT n.id, n.e164, n.e164_source, n.note FROM number_bindings b JOIN numbers n ON n.id = b.number_id
				WHERE b.sim_id = ? AND b.to_at IS NULL`,
			)
			.get(simId);
		if (!row) return null;
		return {
			id: row.id,
			e164: row.e164,
			source: row.e164_source,
			note: row.note,
		};
	}

	private update(
		table: "phones" | "sims" | "numbers",
		id: number,
		columns: [string, unknown][],
	): void {
		if (columns.length === 0) return;
		const set = columns.map(([column]) => `${column} = ?`).join(", ");
		this.db.run(`UPDATE ${table} SET ${set} WHERE id = ?`, [
			...columns.map(([, value]) => value as string | number | null),
			id,
		]);
	}

	private audit(
		login: string | null,
		entity: string,
		entityId: number,
		changes: unknown,
	): void {
		this.db.run(
			"INSERT INTO inventory_edits (at, login, entity, entity_id, changes_json) VALUES (?, ?, ?, ?, ?)",
			[
				new Date().toISOString(),
				login,
				entity,
				entityId,
				JSON.stringify(changes),
			],
		);
	}
}
