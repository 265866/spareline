import { expect, test } from "bun:test";
import { fixture, ReplayShell } from "../../test/replay-shell";
import { openDb } from "../db/db";
import {
	IDENTITY_SCRIPT,
	type PhoneIdentity,
	probeIdentity,
} from "../devices/probe";
import { Inventory } from "../inventory/store";
import { applyIdentity, recordPhoneState } from "../inventory/sync";

// The inventory half of SIM hotplug (D11), driven by identity reads as the
// device manager makes them after the monitor's SIM watch asks for a refresh.
// A SIM cannot be pulled on demand, so the removed state is the recorded
// identity with its slot emptied, as probe.ts reports an ABSENT slot.

const B30 = "SPARELINE00001";

async function recordedIdentity(): Promise<PhoneIdentity> {
	const shell = new ReplayShell()
		.answer(IDENTITY_SCRIPT, await fixture("nuu-s6702x", "identity.txt"))
		.answerPrefix(
			"printf '\\n@@sl:imei:0\\n'",
			await fixture("nuu-s6702x", "subscriber.txt"),
		);
	return probeIdentity(shell, B30);
}

test("the same SIM back reopens a placement and keeps its number", async () => {
	const db = openDb(":memory:");
	const inventory = new Inventory(db);
	const identity = await recordedIdentity();
	const phoneId = recordPhoneState(db, {
		serial: B30,
		state: "device",
		site: "home",
		at: "2026-10-22T08:00:00.000Z",
	}).phoneId;
	const kinds = (events: { kind: string }[]) => events.map((e) => e.kind);
	expect(
		kinds(applyIdentity(db, phoneId, identity, "2026-10-22T08:00:00.000Z")),
	).toEqual(["sim.added", "sim.placed", "number.bound"]);

	const removed = structuredClone(identity);
	removed.slots[0] = { slot: 0, state: "empty", simState: "ABSENT" };
	removed.sims = [];
	expect(
		kinds(applyIdentity(db, phoneId, removed, "2026-10-22T08:10:00.000Z")),
	).toEqual(["sim.removed"]);
	expect(inventory.findNumber("+15125550101")?.phone?.removedAt).toBe(
		"2026-10-22T08:10:00.000Z",
	);

	// Back in: not a new SIM, no new number, one new open placement.
	expect(
		kinds(applyIdentity(db, phoneId, identity, "2026-10-22T08:20:00.000Z")),
	).toEqual(["sim.placed"]);
	const number = inventory.findNumber("+15125550101");
	expect(number?.id).toBe(1);
	expect(number?.phone).toMatchObject({
		serial: B30,
		slot: 0,
		removedAt: null,
	});
	expect(
		db.query("SELECT from_at, to_at FROM sim_placements ORDER BY id").all(),
	).toEqual([
		{ from_at: "2026-10-22T08:00:00.000Z", to_at: "2026-10-22T08:10:00.000Z" },
		{ from_at: "2026-10-22T08:20:00.000Z", to_at: null },
	]);
	db.close();
});
