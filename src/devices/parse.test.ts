import { describe, expect, test } from "bun:test";
import { fixture } from "../../test/replay-shell";
import {
	parseBattery,
	parseIsub,
	parseKeyValues,
	parseParcelString,
	parseRegistry,
	parseSimStates,
	splitSections,
} from "./parse";

// Recorded from the B30 (NUU S6702X, Android 14) with one T-Mobile SIM in slot
// 0, slot 1 empty, out of service where it was recorded. Identifiers are fake.
const B30 = "nuu-s6702x";
const section = async (file: string, name: string) => {
	const value = splitSections(await fixture(B30, file)).get(name);
	if (value === undefined) throw new Error(`missing section ${name}`);
	return value;
};

test("splitSections", () => {
	const sections = splitSections("\n@@sl:a\none\ntwo\n\n@@sl:b\r\nthree\n");
	expect([...sections]).toEqual([
		["a", "one\ntwo"],
		["b", "three"],
	]);
});

test("props", async () => {
	const props = parseKeyValues(await section("identity.txt", "props"));
	expect(props["ro.product.model"]).toBe("S6702X");
	expect(props["ro.build.version.release"]).toBe("14");
	expect(props["ro.build.version.sdk"]).toBe("34");
	expect(props["gsm.sim.state"]).toBe("LOADED,ABSENT");
	expect(parseSimStates(props["gsm.sim.state"] ?? "")).toEqual([
		"LOADED",
		"ABSENT",
	]);
});

test("dumpsys battery", async () => {
	expect(parseBattery(await section("health.txt", "battery"))).toEqual({
		level: 85,
		temperatureDeciC: 221,
		voltageMv: 4103,
		status: "not_charging",
		health: "good",
		plugged: ["usb"],
	});
	expect(parseBattery("Can't find service: battery")).toBeNull();
});

test("dumpsys isub", async () => {
	const isub = parseIsub(await section("identity.txt", "isub"));
	expect(isub.activeModemCount).toBe(2);
	expect([...isub.slotSubIds]).toEqual([[0, 1]]);
	expect([...isub.simStates]).toEqual([
		[0, "LOADED"],
		[1, "ABSENT"],
	]);
	expect(isub.allLoaded).toBe(true);
	expect(isub.defaultDataSubId).toBe(1);
	expect(isub.defaultSmsSubId).toBe(1);
	expect(isub.active).toEqual([
		{
			subId: 1,
			slot: 0,
			iccidPrefix: "890155500",
			displayName: "T-Mobile",
			// carrierName reflects service, and values may contain spaces.
			carrierName: "Emergency calls only",
			mcc: "310",
			mnc: "240",
			countryIso: "us",
		},
	]);
});

describe("telephony.registry", () => {
	test("out of service, slot 1 without a SIM", async () => {
		const [slot0, slot1, ...rest] = parseRegistry(
			await section("health.txt", "registry"),
		);
		expect(rest).toEqual([]);
		expect(slot0).toEqual({
			slot: 0,
			voice: "out_of_service",
			data: "out_of_service",
			operator: null,
			emergencyOnly: true,
			signalLevel: 1,
			signalType: "LTE",
			signalDbm: -122,
			dataEnabled: false,
			dataConnected: false,
		});
		expect(slot1).toMatchObject({
			slot: 1,
			voice: "power_off",
			data: "power_off",
			signalLevel: 0,
			signalType: null,
			signalDbm: null,
			dataConnected: null,
		});
	});

	// mServiceState copied from the phone's own registry log while it was in
	// service (2026-10-21 19:41:59); the rest is the current recording.
	test("in service", async () => {
		const [slot0] = parseRegistry(
			await section("health-in-service.txt", "registry"),
		);
		expect(slot0).toMatchObject({
			voice: "in_service",
			data: "in_service",
			operator: "T-Mobile",
			emergencyOnly: false,
		});
	});
});

describe("service call Parcels", () => {
	test("strings, multi-line", async () => {
		expect(
			parseParcelString(await section("subscriber.txt", "imei:0")),
		).toEqual({
			kind: "value",
			value: "350000000000006",
		});
		expect(
			parseParcelString(await section("subscriber.txt", "iccid:1")),
		).toEqual({
			kind: "value",
			value: "8901555000000000001",
		});
		expect(
			parseParcelString(await section("subscriber.txt", "number:1")),
		).toEqual({
			kind: "value",
			value: "15125550101",
		});
	});

	test("a null string on one line", () => {
		expect(
			parseParcelString("Result: Parcel(\t00000000 ffffffff   '........')"),
		).toEqual({
			kind: "value",
			value: null,
		});
	});

	// Parcel::writeException: an EX_SECURITY (-1) code and a String16 message.
	test("a binder exception", () => {
		expect(
			parseParcelString(
				"Result: Parcel(\n0x00000000: ffffffff 00000003 00690068 00000021 '....h.i.!...')",
			),
		).toEqual({ kind: "exception", code: -1, message: "hi!" });
	});

	test("tool errors", async () => {
		const errors = await fixture(B30, "service-call-errors.txt");
		const sections = splitSections(errors);
		expect(parseParcelString(sections.get("unknown-code") ?? "")).toEqual({
			kind: "invalid",
			reason: 'Error: 0xffffffffffffffb6 "Not a data message"',
		});
		expect(parseParcelString(sections.get("no-service") ?? "")).toEqual({
			kind: "invalid",
			reason: "service: Service nosuchservice does not exist",
		});
	});

	test("a length longer than the reply is invalid, not a crash", () => {
		expect(
			parseParcelString("Result: Parcel(00000000 00000010 00310031 '....')")
				.kind,
		).toBe("invalid");
	});
});
