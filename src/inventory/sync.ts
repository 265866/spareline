import type { Database } from "bun:sqlite";
import type { DeviceState } from "../adb/device-list";
import type { PhoneIdentity } from "../devices/probe";

// How a phone looks to the owner, derived from its adb state.
export type PhoneStatus =
	| "online"
	| "connecting"
	| "needs_auth"
	| "no_permissions"
	| "offline"
	| "other";

export function phoneStatus(state: DeviceState | "gone"): PhoneStatus {
	switch (state) {
		case "device":
			return "online";
		case "unauthorized":
			return "needs_auth";
		case "no_permissions":
			return "no_permissions";
		case "connecting":
		case "authorizing":
			return "connecting";
		case "offline":
		case "gone":
		case "detached":
		case "unknown":
			return "offline";
		default:
			return "other";
	}
}

export type NewEvent = {
	phoneId: number | null;
	kind: string;
	detail: Record<string, unknown>;
};

export type DeviceEventRow = {
	id: number;
	phoneId: number | null;
	serial: string | null;
	at: string;
	kind: string;
	detail: Record<string, unknown>;
};

export function insertEvents(
	db: Database,
	events: NewEvent[],
	at: string,
): DeviceEventRow[] {
	const insert = db.query<
		{ id: number },
		[number | null, string, string, string]
	>(
		"INSERT INTO device_events (phone_id, at, kind, detail_json) VALUES (?, ?, ?, ?) RETURNING id",
	);
	const serialOf = db.query<{ serial: string }, [number]>(
		"SELECT serial FROM phones WHERE id = ?",
	);
	return events.map((event) => {
		const row = insert.get(
			event.phoneId,
			at,
			event.kind,
			JSON.stringify(event.detail),
		);
		if (!row) throw new Error("device event insert returned no id");
		return {
			id: row.id,
			phoneId: event.phoneId,
			serial:
				event.phoneId === null
					? null
					: (serialOf.get(event.phoneId)?.serial ?? null),
			at,
			kind: event.kind,
			detail: event.detail,
		};
	});
}

// Records a phone's adb state. Creates the phone row on first sight, in any
// state, so a phone waiting for "Allow USB debugging" shows up at once.
export function recordPhoneState(
	db: Database,
	input: {
		serial: string;
		state: DeviceState | "gone";
		site: string;
		at: string;
		detail?: Record<string, unknown>;
	},
): { phoneId: number; events: NewEvent[] } {
	return db.transaction(() => {
		const existing = db
			.query<{ id: number; last_state: string }, [string]>(
				"SELECT id, last_state FROM phones WHERE serial = ?",
			)
			.get(input.serial);
		const online = input.state === "device" ? input.at : null;
		const after = phoneStatus(input.state);
		const events: NewEvent[] = [];
		let phoneId: number;
		let before: PhoneStatus | null = null;
		if (existing) {
			phoneId = existing.id;
			before = phoneStatus(existing.last_state as DeviceState);
			// Transient states (adb's "connecting"/"authorizing" during a replug)
			// are neither stored nor events, so a replug yields one offline and
			// one online event, compared against the last settled state.
			if (after !== "connecting")
				db.run(
					"UPDATE phones SET last_state = ?, last_online_at = coalesce(?, last_online_at) WHERE id = ?",
					[input.state, online, phoneId],
				);
		} else {
			const row = db
				.query<{ id: number }, [string, string, string, string, string | null]>(
					"INSERT INTO phones (serial, site, last_state, first_seen_at, last_online_at) VALUES (?, ?, ?, ?, ?) RETURNING id",
				)
				.get(input.serial, input.site, input.state, input.at, online);
			if (!row) throw new Error("phone insert returned no id");
			phoneId = row.id;
			events.push({
				phoneId,
				kind: "phone.added",
				detail: { serial: input.serial, state: input.state },
			});
		}
		if (after !== before && after !== "connecting") {
			const kind = {
				online: "phone.online",
				needs_auth: "phone.needs_auth",
				no_permissions: "phone.no_permissions",
				offline: "phone.offline",
				other: "phone.other_state",
			}[after];
			events.push({
				phoneId,
				kind,
				detail: { state: input.state, ...input.detail },
			});
		}
		return { phoneId, events };
	})();
}

// Applies a fresh identity probe: device facts, which SIM is in which slot,
// and which number each SIM carries. Slots in an "unknown" state are left
// as they were, so a SIM that is still loading is not recorded as removed.
export function applyIdentity(
	db: Database,
	phoneId: number,
	identity: PhoneIdentity,
	at: string,
): NewEvent[] {
	return db.transaction(() => {
		const events: NewEvent[] = [];
		db.run(
			`UPDATE phones SET
				manufacturer = coalesce(?, manufacturer), model = coalesce(?, model),
				android_version = coalesce(?, android_version), sdk = coalesce(?, sdk),
				build_fingerprint = coalesce(?, build_fingerprint),
				imei1 = coalesce(?, imei1), imei2 = coalesce(?, imei2)
			WHERE id = ?`,
			[
				identity.manufacturer,
				identity.model,
				identity.androidVersion,
				identity.sdk,
				identity.fingerprint,
				identity.imeis[0] ?? null,
				identity.imeis[1] ?? null,
				phoneId,
			],
		);
		const openInSlot = db.query<
			{ id: number; sim_id: number; sub_id: number; iccid: string },
			[number, number]
		>(
			`SELECT p.id, p.sim_id, p.sub_id, s.iccid FROM sim_placements p JOIN sims s ON s.id = p.sim_id
			WHERE p.phone_id = ? AND p.slot = ? AND p.to_at IS NULL`,
		);
		const close = (
			placement: { id: number; sim_id: number; iccid: string },
			slot: number,
			onPhone: number,
		) => {
			db.run("UPDATE sim_placements SET to_at = ? WHERE id = ?", [
				at,
				placement.id,
			]);
			events.push({
				phoneId: onPhone,
				kind: "sim.removed",
				detail: { slot, simId: placement.sim_id, iccid: placement.iccid },
			});
		};
		for (const slot of identity.slots) {
			const open = openInSlot.get(phoneId, slot.slot);
			if (slot.state === "empty") {
				if (open) close(open, slot.slot, phoneId);
				continue;
			}
			if (slot.state !== "present") continue;
			const sim = identity.sims.find(
				(candidate) => candidate.slot === slot.slot,
			);
			if (!sim?.iccid) continue;
			const simId = upsertSim(db, sim.iccid, sim.carrier, at, phoneId, events);
			if (open?.sim_id === simId) {
				if (open.sub_id !== sim.subId)
					db.run("UPDATE sim_placements SET sub_id = ? WHERE id = ?", [
						sim.subId,
						open.id,
					]);
			} else {
				if (open) close(open, slot.slot, phoneId);
				const elsewhere = db
					.query<
						{
							id: number;
							sim_id: number;
							phone_id: number;
							slot: number;
							iccid: string;
						},
						[number]
					>(
						`SELECT p.id, p.sim_id, p.phone_id, p.slot, s.iccid FROM sim_placements p JOIN sims s ON s.id = p.sim_id
						WHERE p.sim_id = ? AND p.to_at IS NULL`,
					)
					.get(simId);
				if (elsewhere) close(elsewhere, elsewhere.slot, elsewhere.phone_id);
				db.run(
					"INSERT INTO sim_placements (sim_id, phone_id, slot, sub_id, from_at) VALUES (?, ?, ?, ?, ?)",
					[simId, phoneId, slot.slot, sim.subId, at],
				);
				events.push({
					phoneId,
					kind: "sim.placed",
					detail: {
						slot: slot.slot,
						subId: sim.subId,
						simId,
						iccid: sim.iccid,
					},
				});
			}
			bindNumber(db, simId, sim.number, at, phoneId, events);
		}
		return events;
	})();
}

function upsertSim(
	db: Database,
	iccid: string,
	carrier: string | null,
	at: string,
	phoneId: number,
	events: NewEvent[],
): number {
	const existing = db
		.query<{ id: number }, [string]>("SELECT id FROM sims WHERE iccid = ?")
		.get(iccid);
	if (existing) {
		db.run(
			"UPDATE sims SET last_seen_at = ?, carrier = coalesce(?, carrier) WHERE id = ?",
			[at, carrier, existing.id],
		);
		return existing.id;
	}
	const row = db
		.query<{ id: number }, [string, string | null, string, string]>(
			"INSERT INTO sims (iccid, carrier, first_seen_at, last_seen_at) VALUES (?, ?, ?, ?) RETURNING id",
		)
		.get(iccid, carrier, at, at);
	if (!row) throw new Error("sim insert returned no id");
	events.push({
		phoneId,
		kind: "sim.added",
		detail: { simId: row.id, iccid, carrier },
	});
	return row.id;
}

type Binding = { id: number; number_id: number; detected_e164: string | null };
type NumberRow = {
	id: number;
	e164: string | null;
	e164_source: "detected" | "manual" | "none";
};

// Keeps the SIM's open number binding in line with the number it reports. A
// number the owner typed is never replaced by detection; a missing detection
// (getters can fail transiently) changes nothing.
function bindNumber(
	db: Database,
	simId: number,
	detected: string | null,
	at: string,
	phoneId: number,
	events: NewEvent[],
): void {
	const binding = db
		.query<Binding, [number]>(
			"SELECT id, number_id, detected_e164 FROM number_bindings WHERE sim_id = ? AND to_at IS NULL",
		)
		.get(simId);
	const numberByE164 = db.query<NumberRow, [string]>(
		"SELECT id, e164, e164_source FROM numbers WHERE e164 = ?",
	);
	const openBinding = (numberId: number) => {
		db.run(
			"UPDATE number_bindings SET to_at = ? WHERE number_id = ? AND to_at IS NULL",
			[at, numberId],
		);
		db.run(
			"INSERT INTO number_bindings (number_id, sim_id, from_at, detected_e164) VALUES (?, ?, ?, ?)",
			[numberId, simId, at, detected],
		);
	};
	const numberFor = (e164: string): number => {
		const found = numberByE164.get(e164);
		if (found) return found.id;
		const row = db
			.query<{ id: number }, [string, string]>(
				"INSERT INTO numbers (e164, e164_source, created_at) VALUES (?, 'detected', ?) RETURNING id",
			)
			.get(e164, at);
		if (!row) throw new Error("number insert returned no id");
		return row.id;
	};

	if (!binding) {
		let numberId: number;
		if (detected) numberId = numberFor(detected);
		else {
			const row = db
				.query<{ id: number }, [string]>(
					"INSERT INTO numbers (e164_source, created_at) VALUES ('none', ?) RETURNING id",
				)
				.get(at);
			if (!row) throw new Error("number insert returned no id");
			numberId = row.id;
		}
		openBinding(numberId);
		events.push({
			phoneId,
			kind: "number.bound",
			detail: { simId, numberId, e164: detected },
		});
		return;
	}
	if (detected === null || binding.detected_e164 === detected) return;
	db.run("UPDATE number_bindings SET detected_e164 = ? WHERE id = ?", [
		detected,
		binding.id,
	]);
	const current = db
		.query<NumberRow, [number]>(
			"SELECT id, e164, e164_source FROM numbers WHERE id = ?",
		)
		.get(binding.number_id);
	if (!current || current.e164_source === "manual" || current.e164 === detected)
		return;
	if (current.e164 === null && !numberByE164.get(detected)) {
		db.run(
			"UPDATE numbers SET e164 = ?, e164_source = 'detected' WHERE id = ?",
			[detected, current.id],
		);
		events.push({
			phoneId,
			kind: "number.detected",
			detail: { simId, numberId: current.id, e164: detected },
		});
		return;
	}
	// The SIM now reports a different number (or one that already has a row):
	// later messages belong to that number, earlier ones stay where they are.
	const numberId = numberFor(detected);
	db.run("UPDATE number_bindings SET to_at = ? WHERE id = ?", [at, binding.id]);
	openBinding(numberId);
	events.push({
		phoneId,
		kind: "number.changed",
		detail: { simId, from: current.e164, to: detected, numberId },
	});
}
