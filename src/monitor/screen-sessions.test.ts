import { afterEach, expect, test } from "bun:test";
import { B30, startMonitor } from "../../test/monitor-phone";
import type { ScreenSessionInfo } from "../screens/manager";
import { screenSessionActive } from "./screen-sessions";

let stop: (() => Promise<void>) | undefined;
afterEach(async () => {
	await stop?.();
	stop = undefined;
});

test("starting and running sessions count; idle entries and other phones do not", () => {
	let sessions: Pick<ScreenSessionInfo, "serial" | "state">[] = [];
	const active = screenSessionActive(() => sessions);
	expect(active(B30)).toBe(false);
	for (const state of ["starting", "running"] as const) {
		sessions = [{ serial: B30, state }];
		expect(active(B30)).toBe(true);
		expect(active("OTHER")).toBe(false);
	}
	sessions = [{ serial: B30, state: "idle" }];
	expect(active(B30)).toBe(false);
});

// The ScreenManager's list() is stood in for: a scrcpy session needs the
// phone's screen, which a unit test cannot open.
test("stay-awake raises no alert while the phone's screen session runs", async () => {
	let sessions: Pick<ScreenSessionInfo, "serial" | "state">[] = [
		{ serial: B30, state: "running" },
	];
	const setup = await startMonitor({
		isScreenSessionActive: screenSessionActive(() => sessions),
	});
	stop = setup.stop;
	const { phone, monitor } = setup;
	phone.set("global", "stay_on_while_plugged_in", "7");
	await monitor.check(B30);
	expect(monitor.alerts().some((a) => a.key.endsWith(":stay_on_off"))).toBe(
		false,
	);
	expect(phone.applied).toEqual([]);
	// The session ends and scrcpy restores the setting.
	sessions = [{ serial: B30, state: "idle" }];
	phone.set("global", "stay_on_while_plugged_in", "0");
	await monitor.check(B30);
	expect(monitor.alerts().some((a) => a.key.endsWith(":stay_on_off"))).toBe(
		false,
	);
	// Left on after the session: an alert, and no automatic change.
	phone.set("global", "stay_on_while_plugged_in", "7");
	await monitor.check(B30);
	expect(monitor.alerts().some((a) => a.key.endsWith(":stay_on_off"))).toBe(
		true,
	);
	expect(phone.applied).toEqual([]);
});
