import { afterEach, expect, test } from "bun:test";
import { mkdtempSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { B30_POLICY } from "../../test/b30-policy";
import { sectionMarker } from "../devices/parse";
import { createPolicy, loadPolicy, Profile } from "./policy";

const PROFILES = join(import.meta.dir, "../../profiles");

let dir: string | undefined;
afterEach(() => {
	if (dir) rmSync(dir, { recursive: true, force: true });
	dir = undefined;
});
const profileFile = (body: string) => {
	dir ??= mkdtempSync(join(tmpdir(), "spareline-profile-"));
	const path = join(dir, "test.json");
	writeFileSync(path, body);
	return path;
};
const sections = (readScript: string) =>
	[...readScript.matchAll(/@@\w+:([\w:]+)\\n/g)].map((match) => match[1]);

test("the B30 example profile turns on every rule group, in check order", () => {
	expect(
		B30_POLICY.rules.map((rule) => [
			rule.id,
			rule.severity,
			rule.autoFix,
			rule.safety ?? false,
		]),
	).toEqual([
		["mobile_data", "critical", true, true],
		["data_roaming", "critical", true, true],
		["wifi_off", "critical", true, true],
		["wifi_wakeup_off", "critical", true, true],
		["wifi_scan_off", "critical", true, true],
		["ble_scan_off", "critical", true, true],
		["bluetooth_off", "critical", true, true],
		["location_off", "critical", true, true],
		["ota_off", "warning", false, false],
		["stay_on_off", "info", false, false],
		["brightness_manual", "info", false, false],
		["brightness_low", "info", false, false],
		["screen_timeout", "info", false, false],
		["data_saver", "warning", false, false],
		["sms_role", "critical", false, false],
		["debloat", "warning", false, false],
		["mms_with_data_off", "critical", false, false],
		["log_buffer", "info", true, false],
	]);
	expect(B30_POLICY.rules.map((rule) => rule.want).slice(9, 13)).toEqual([
		"global stay_on_while_plugged_in = 0",
		"system screen_brightness_mode = 0",
		"system screen_brightness = 10",
		"system screen_off_timeout = 15000",
	]);
	expect(B30_POLICY.radiosOff).toEqual({ dataThresholdBytes: 1_000_000 });
	expect(sections(B30_POLICY.readScript)).toEqual([
		"settings:global",
		"settings:secure",
		"settings:system",
		"isub",
		"registry",
		"datasaver",
		"smsrole",
		"bloat",
		"radiobuf",
		"clock",
		"netstats",
	]);
	expect(B30_POLICY.lastSection).toBe("netstats");
});

test("a profile turns on only the groups it names, and reads only what they need", () => {
	const empty = createPolicy("empty", Profile.parse({}));
	expect(empty.rules).toEqual([]);
	expect(empty.radiosOff).toBeNull();
	expect(empty.namespaces).toEqual([]);
	expect(sections(empty.readScript)).toEqual(["isub", "registry"]);

	const screen = createPolicy(
		"screen",
		Profile.parse({ screen: { timeoutSeconds: 30, autoBrightness: true } }),
	);
	expect(screen.rules.map((rule) => [rule.id, rule.want])).toEqual([
		["brightness_manual", "system screen_brightness_mode = 1"],
		["screen_timeout", "system screen_off_timeout = 30000"],
	]);
	expect(screen.readScript).toContain(
		"settings list system | grep -E '^(screen_brightness_mode|screen_off_timeout)='",
	);
	expect(sections(screen.readScript)).toEqual([
		"settings:system",
		"isub",
		"registry",
	]);
	expect(screen.namespaces).toEqual(["system"]);
	expect(screen.readScript).toContain(sectionMarker("registry"));
});

test("every shipped profile loads, and no-data keeps radios off without device tuning", () => {
	const names = readdirSync(PROFILES).filter((file) => file.endsWith(".json"));
	expect(names.sort()).toEqual(["no-data.json", "nuu-b30-no-data.json"]);
	for (const name of names)
		expect(loadPolicy(join(PROFILES, name)).name).toBe(
			name.replace(".json", ""),
		);
	const noData = loadPolicy(join(PROFILES, "no-data.json"));
	expect(noData.radiosOff).toEqual({ dataThresholdBytes: 1_000_000 });
	expect(noData.rules.map((rule) => rule.id)).toEqual([
		"mobile_data",
		"data_roaming",
		"wifi_off",
		"wifi_wakeup_off",
		"wifi_scan_off",
		"ble_scan_off",
		"bluetooth_off",
		"location_off",
		"ota_off",
		"data_saver",
		"mms_with_data_off",
	]);
});

test("a bad profile is refused with every problem named", () => {
	expect(() => loadPolicy(profileFile("{ not json"))).toThrow(
		"cannot read policy profile",
	);
	expect(() => loadPolicy(join(PROFILES, "missing-profile.json"))).toThrow(
		"cannot read policy profile",
	);
	let message = "";
	try {
		loadPolicy(
			profileFile(
				JSON.stringify({
					radiosOf: {},
					removeApps: ["not a package; rm -rf /"],
					screen: { brightness: 300 },
				}),
			),
		);
	} catch (error) {
		message = String(error);
	}
	expect(message).toContain("invalid policy profile");
	expect(message).toContain("radiosOf");
	expect(message).toContain("removeApps");
	expect(message).toContain("screen.brightness");
});
