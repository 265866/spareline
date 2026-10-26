import { describe, expect, test } from "bun:test";
import { fixture } from "../../test/replay-shell";
import {
	activitySignature,
	decodeHex,
	parseContentRows,
	parseIsms,
} from "./parse";
import { SMS_COLUMNS } from "./provider";

describe("parseContentRows", () => {
	test("the B30's recorded inbox: every row, NULL-free, bodies decode as UTF-8", async () => {
		const { rows, malformed } = parseContentRows(
			await fixture("nuu-s6702x", "sms-provider.txt"),
			SMS_COLUMNS,
		);
		expect(malformed).toEqual([]);
		expect(rows.map((row) => row._id)).toEqual([
			"4",
			"5",
			"6",
			"7",
			"8",
			"9",
			"10",
			"11",
		]);
		expect(rows[0]).toMatchObject({
			thread_id: "2",
			address: "550",
			date: "1792637380314",
			date_sent: "1792623882000",
			sub_id: "1",
			type: "1",
		});
		expect(rows[1]?.address).toBe("+15125550151");
		expect(decodeHex(rows[7]?.["hex(body)"] ?? null)).toBe(
			"Fake text 8, is the café table still for sale? ✓",
		);
	});

	test("empty results and NULLs", () => {
		expect(parseContentRows("No result found.\n", SMS_COLUMNS)).toEqual({
			rows: [],
			malformed: [],
		});
		const { rows } = parseContentRows(
			"Row: 0 _id=1, mid=2047483647, ct=NULL, hex(text)=\n",
			["_id", "mid", "ct", "hex(text)"],
		);
		expect(rows).toEqual([
			{ _id: "1", mid: "2047483647", ct: null, "hex(text)": "" },
		]);
	});

	// Hand-written lines: the B30's inbox has no address with a comma or a
	// newline, but the tool prints values unescaped, so both must survive.
	test("values with commas, '=' and newlines stay whole", () => {
		const output = [
			"Row: 0 _id=12, address=Smith, J=x, date=5, hex(body)=4869",
			"Row: 1 _id=13, address=line one",
			"",
			"line two, date=6, hex(body)=",
			"",
		].join("\n");
		const { rows, malformed } = parseContentRows(output, [
			"_id",
			"address",
			"date",
			"hex(body)",
		]);
		expect(malformed).toEqual([]);
		expect(rows).toEqual([
			{ _id: "12", address: "Smith, J=x", date: "5", "hex(body)": "4869" },
			{
				_id: "13",
				// The blank line inside the value survives.
				address: "line one\n\nline two",
				date: "6",
				"hex(body)": "",
			},
		]);
	});

	// An unexpected extra column cannot be told apart from a comma in a value;
	// integer columns are validated when rows become messages.
	test("a row missing a column or out of order is reported, not guessed", () => {
		const { rows, malformed } = parseContentRows(
			"Row: 0 _id=1, date=2\nRow: 1 _id=2\nRow: 2 date=3, _id=4\n",
			["_id", "date"],
		);
		expect(rows).toEqual([{ _id: "1", date: "2" }]);
		expect(malformed).toEqual(["Row: 1 _id=2", "Row: 2 date=3, _id=4"]);
	});
});

test("decodeHex", () => {
	expect(decodeHex("F09F918B2C20686921")).toBe("👋, hi!");
	expect(decodeHex("")).toBe("");
	expect(decodeHex(null)).toBe("");
	expect(() => decodeHex("ABC")).toThrow("odd length");
	expect(() => decodeHex("ZZ")).toThrow();
});

describe("parseIsms", () => {
	test("the B30's recorded trigger output", async () => {
		const activity = parseIsms(await fixture("nuu-s6702x", "isms.txt"));
		// GSM and CDMA handlers, printed once for sub 1 and once for sub -1.
		expect(activity.handlerRecords).toEqual([105, 2, 105, 2]);
		expect(activity.lastEventAt).toBe("10-21 19:50:27.278");
		expect(activity.deliveries).toBe(16);
		expect(activity.lastDeliveryAt).toBe("2026-10-21T19:50:27.267498");
	});

	// A stand-in for the next read after a text arrives: the recording with
	// the records a new delivery adds. The phone is out of service, so a real
	// second read with a new text cannot be produced on demand.
	test("a new inbound SMS changes the signature; a re-read does not", async () => {
		const before = await fixture("nuu-s6702x", "isms.txt");
		const after = before
			.replace("total records=105", "total records=111")
			.replace(
				"      CdmaInboundSmsHandler:",
				"       rec[19]: time=10-22 08:01:02.003 processed=MtkIdleState org=MtkIdleState dest=MtkDeliveringState what=EVENT_NEW_SMS\n      CdmaInboundSmsHandler:",
			);
		const first = activitySignature(parseIsms(before));
		expect(activitySignature(parseIsms(before))).toBe(first);
		const next = parseIsms(after);
		expect(next.lastEventAt).toBe("10-22 08:01:02.003");
		expect(activitySignature(next)).not.toBe(first);
	});

	test("no inbound handler output at all", () => {
		expect(parseIsms("")).toEqual({
			handlerRecords: [],
			lastEventAt: null,
			deliveries: 0,
			lastDeliveryAt: null,
		});
	});
});
