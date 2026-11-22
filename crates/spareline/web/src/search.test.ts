import { expect, test } from "bun:test";
import type { NumberView } from "./api";
import { matchesNumber, visibleNumbers } from "./search";

const number: NumberView = {
	id: 1,
	e164: "+15125550101",
	source: "detected",
	note: "Facebook ad, blue couch",
	detectedE164: "+15125550101",
	confirmedBy: null,
	createdAt: "2026-10-21T00:00:00.000Z",
	sim: { id: 1, iccid: "8901555000000000001", carrier: "T-Mobile" },
	phone: {
		serial: "SPARELINE00001",
		model: "S6702X",
		slot: 0,
		removedAt: null,
	},
	phoneStatus: "online",
	radio: null,
	messageCount: 2,
	lastMessage: {
		seq: 2,
		from: "+15125550199",
		preview: "Is the couch still available?",
		receivedAt: "2026-10-21T19:50:27.282Z",
	},
};

test("finds a number written any way", () => {
	for (const query of [
		"5125550101",
		"+15125550101",
		"+1512",
		"(512) 555",
		"(512) 555-0101",
		"512-555-0101",
		"512.555.0101",
		"+1 512 555 0101",
		"0101",
	])
		expect(matchesNumber(number, query)).toBe(true);
	expect(matchesNumber(number, "5125550102")).toBe(false);
	expect(matchesNumber(number, "(512) 555-0102")).toBe(false);
});

test("finds the note, phone, and SIM slot, ignoring case", () => {
	for (const query of [
		"facebook AD",
		"blue couch",
		"spareline00",
		"s6702x",
		"SIM 1",
	])
		expect(matchesNumber(number, query)).toBe(true);
	expect(matchesNumber(number, "craigslist")).toBe(false);
	expect(matchesNumber(number, "SIM 2")).toBe(false);
});

test("the last text and its sender are not searched; the page does not show them", () => {
	for (const query of ["still available", "5550199", "(512) 555-0199"])
		expect(matchesNumber(number, query)).toBe(false);
});

test("an empty query matches; text with digits is not a number search", () => {
	expect(matchesNumber(number, "")).toBe(true);
	expect(matchesNumber(number, "   ")).toBe(true);
	// "ad 512" is not phone-like, and no field holds it as written.
	expect(matchesNumber(number, "ad 512")).toBe(false);
	// A query cannot match across two fields.
	expect(matchesNumber(number, "couch S6702X")).toBe(false);
});

test("an undetected number with no SIM still matches its note", () => {
	const bare: NumberView = {
		...number,
		e164: null,
		detectedE164: null,
		source: "none",
		phone: null,
		lastMessage: null,
		note: "spare",
	};
	expect(matchesNumber(bare, "SPARE")).toBe(true);
	expect(matchesNumber(bare, "not detected")).toBe(true);
	expect(matchesNumber(bare, "512")).toBe(false);
});

test("a row open for editing stays visible when the search or its data stops matching", () => {
	const other: NumberView = {
		...number,
		id: 2,
		e164: "+15125550102",
		detectedE164: "+15125550102",
		note: "Craigslist",
		lastMessage: null,
	};
	const ids = (list: NumberView[]) => list.map((item) => item.id);
	const none = new Set<number>();
	expect(ids(visibleNumbers([number, other], "craigslist", none))).toEqual([2]);
	// Number 1's note is open: a search typed mid-edit keeps it.
	const editingOne = new Set([1]);
	expect(
		ids(visibleNumbers([number, other], "craigslist", editingOne)),
	).toEqual([1, 2]);
	expect(ids(visibleNumbers([number, other], "zzz", editingOne))).toEqual([1]);
	// A refresh that changes its note away from the search keeps it too.
	const refreshed: NumberView = { ...number, note: "Sold" };
	expect(ids(visibleNumbers([refreshed, other], "blue couch", none))).toEqual(
		[],
	);
	expect(
		ids(visibleNumbers([refreshed, other], "blue couch", editingOne)),
	).toEqual([1]);
	// Closing the editor lets the search hide it again.
	expect(ids(visibleNumbers([number, other], "zzz", none))).toEqual([]);
});
