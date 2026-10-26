import { expect, test } from "bun:test";
import { parseDeviceList } from "./device-list";

test("a long-format line from the B30", () => {
	expect(
		parseDeviceList(
			"SPARELINE00001         device usb:1-1 product:S6702X1 model:S6702X device:S6702XA transport_id:1\n",
		),
	).toEqual([
		{
			serial: "SPARELINE00001",
			state: "device",
			rawState: "device",
			usb: "1-1",
			product: "S6702X1",
			model: "S6702X",
			device: "S6702XA",
			transportId: 1,
		},
	]);
});

test("a reconnecting transport reports only its usb path and id", () => {
	expect(
		parseDeviceList(
			"SPARELINE00001         authorizing usb:1-1 transport_id:2\n",
		),
	).toEqual([
		{
			serial: "SPARELINE00001",
			state: "authorizing",
			rawState: "authorizing",
			usb: "1-1",
			transportId: 2,
		},
	]);
});

test("an empty list", () => {
	expect(parseDeviceList("")).toEqual([]);
});

// adb 34's format for these states (transport.cpp, usb_linux.cpp); no phone
// in this state was available to record.
test("no permissions keeps adb's help text but normalizes the state", () => {
	const [entry] = parseDeviceList(
		"SPARELINE00001         no permissions (missing udev rules? user is in the plugdev group); see [http://developer.android.com/tools/device.html] usb:1-1 transport_id:3\n",
	);
	expect(entry?.state).toBe("no_permissions");
	expect(entry?.rawState).toStartWith("no permissions (missing udev rules?");
	expect(entry?.usb).toBe("1-1");
	expect(entry?.transportId).toBe(3);
});

test("unauthorized and unknown states, several lines", () => {
	const entries = parseDeviceList(
		"AAA                    unauthorized usb:1-2 transport_id:4\nBBB                    weird-new-state usb:1-3 transport_id:5\n",
	);
	expect(entries.map((e) => [e.serial, e.state, e.rawState])).toEqual([
		["AAA", "unauthorized", "unauthorized"],
		["BBB", "unknown", "weird-new-state"],
	]);
});
