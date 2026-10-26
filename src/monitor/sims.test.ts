import { expect, test } from "bun:test";
import { fixture } from "../../test/replay-shell";
import { diffSlots, SIM_LOG_COMMAND, SIM_LOG_PATTERN } from "./sims";

const map = (entries: [number, number][]) => new Map(entries);

test("slot diffs: inserted, removed, swapped, unchanged", () => {
	expect(diffSlots(map([[0, 1]]), map([[0, 1]]))).toEqual([]);
	expect(
		diffSlots(
			map([[0, 1]]),
			map([
				[0, 1],
				[1, 2],
			]),
		),
	).toEqual([{ slot: 1, kind: "inserted", from: null, to: 2 }]);
	expect(
		diffSlots(
			map([
				[0, 1],
				[1, 2],
			]),
			map([[1, 2]]),
		),
	).toEqual([{ slot: 0, kind: "removed", from: 1, to: null }]);
	expect(diffSlots(map([[0, 1]]), map([[0, 3]]))).toEqual([
		{ slot: 0, kind: "swapped", from: 1, to: 3 },
	]);
	// The same card back in another slot keeps its sub id.
	expect(diffSlots(map([[0, 1]]), map([[1, 1]]))).toEqual([
		{ slot: 0, kind: "removed", from: 1, to: null },
		{ slot: 1, kind: "inserted", from: null, to: 1 },
	]);
});

// logcat -e uses ECMAScript regex, which JavaScript's RegExp matches.
const pattern = new RegExp(SIM_LOG_PATTERN);

test("the SIM log filter ignores the B30's usual radio log traffic", async () => {
	const lines = (await fixture("nuu-s6702x", "radio-log.txt"))
		.split("\n")
		.filter(Boolean);
	expect(lines.some((line) => line.includes("RILJ"))).toBe(true);
	expect(lines.filter((line) => pattern.test(line))).toEqual([]);
});

test("the SIM log filter matches SIM state lines in the RIL's recorded format", () => {
	// No SIM can be inserted on demand, so these follow the measured RILJ
	// format ("[UNSL]< NAME", "[UNSL]< MTK: RIL_UNSOL_...", "[1234]> NAME")
	// with AOSP's and MTK's SIM message names.
	for (const line of [
		"D/RILJ    ( 1937): [UNSL]< UNSOL_RESPONSE_SIM_STATUS_CHANGED [PHONE0]",
		"D/RILJ    ( 1937): [4801]> GET_SIM_STATUS [PHONE1]",
		"D/RILJ    ( 1937): [UNSL]< MTK: RIL_UNSOL_SIM_PLUG_OUT [PHONE1]",
		"D/RILJ    ( 1937): [UNSL]< MTK: RIL_UNSOL_SIM_PLUG_IN [PHONE1]",
		"D/RILJ    ( 1937): [UNSL]< MTK: RIL_UNSOL_SIM_MISSING [PHONE0]",
		"D/RILJ    ( 1937): [UNSL]< UNSOL_SIM_SLOT_STATUS_CHANGED",
		"I/AT      ( 1434): [1] AT< +ESIMS: 0,11 (RIL_URC2_READER, tid:1)",
	])
		expect([line, pattern.test(line)]).toEqual([line, true]);
	expect(SIM_LOG_COMMAND).toBe(
		`logcat -b radio -v brief -T 1 -e '${SIM_LOG_PATTERN}'`,
	);
});
