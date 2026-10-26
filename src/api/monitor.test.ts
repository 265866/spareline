import { afterEach, expect, test } from "bun:test";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { B30, startMonitor } from "../../test/monitor-phone";
import { signedIn, testSession } from "../../test/session";
import { Bus } from "../bus";
import { Inventory } from "../inventory/store";
import { MessageStore } from "../sms/store";
import { createApp } from "./app";
import type { ProvisionReportView } from "./monitor-schema";
import type { StatusView } from "./schema";

// The monitor runs over a MonitorPhone stand-in (test/monitor-phone.ts explains
// why); the app, routes, validation, and database are real.

let stop: (() => Promise<void>) | undefined;
afterEach(async () => {
	await stop?.();
});

async function setup(options: Parameters<typeof startMonitor>[0] = {}) {
	const monitor = await startMonitor(options);
	stop = monitor.stop;
	const session = testSession(monitor.db);
	const app = createApp({
		auth: session.auth,
		inventory: new Inventory(monitor.db),
		devices: monitor.devices,
		events: monitor.bus,
		// Texts are not under test; nothing is written here.
		messages: {
			store: new MessageStore(monitor.db),
			bus: new Bus(),
			attachmentsDir: join(tmpdir(), "spareline-monitor-test-attachments"),
		},
		monitor: monitor.monitor,
		webDir: null,
		version: "test",
	});
	signedIn(app, session.cookie);
	// As the web client sends them: a JSON body, even an empty one.
	const call = async (
		method: string,
		path: string,
		headers: Record<string, string> = method === "POST"
			? { "content-type": "application/json" }
			: {},
	) =>
		app.handle(
			new Request(`http://localhost${path}`, {
				method,
				headers,
				body: headers["content-type"] ? "{}" : undefined,
			}),
		);
	return { ...monitor, call };
}

test("the status view carries rule results, data use, and alerts", async () => {
	const { call } = await setup();
	const response = await call("GET", "/api/v1/status");
	expect(response.status).toBe(200);
	const view = (await response.json()) as StatusView;
	expect(view.dataThresholdBytes).toBe(1_000_000);
	expect(view.profile?.name).toBe("nuu-b30-no-data");
	expect(view.profile?.setUp).toContain("Unneeded apps removed");
	expect(view.profile?.removesApps).toBe(true);
	expect(view.profile?.setUp).toContain(
		"Google Messages is the default SMS app",
	);
	const policy = view.phones[0]?.policy;
	expect(policy?.check?.results).toHaveLength(18);
	expect(
		policy?.check?.results.find((result) => result.id === "mms_with_data_off"),
	).toMatchObject({ ok: true, fixable: false, severity: "critical" });
	// The connect check enlarged the recorded 256 KiB log buffer.
	expect(
		policy?.check?.results.find((result) => result.id === "log_buffer"),
	).toMatchObject({ ok: true, autoFix: true, observed: "8 MiB" });
	expect(policy?.dataAlarm).toBe(false);
	expect(view.alerts).toEqual([]);
	expect(await (await call("GET", "/api/v1/health")).json()).toMatchObject({
		ok: true,
		alerts: 0,
	});
});

test("Provision and check run on demand; an offline phone is refused", async () => {
	const { call, phone } = await setup();
	phone.set("system", "screen_brightness_mode", "1");
	phone.bigRadioBuffer();
	const first = await call("POST", `/api/v1/phones/${B30}/provision`);
	expect(first.status).toBe(200);
	const report = (await first.json()) as ProvisionReportView;
	expect(report).toMatchObject({ ok: true, error: null });
	expect(report.steps.map((step) => step.rule)).toEqual(["brightness_manual"]);
	const again = (await (
		await call("POST", `/api/v1/phones/${B30}/provision`)
	).json()) as ProvisionReportView;
	expect(again.steps).toEqual([]);
	expect(phone.applied).toHaveLength(1);

	phone.set("global", "ble_scan_always_enabled", "1");
	const check = await call("POST", `/api/v1/phones/${B30}/check`);
	expect(check.status).toBe(200);
	expect(((await check.json()) as { reverted: unknown[] }).reverted).toEqual([
		{ rule: "ble_scan_off", before: "1", after: "0", fixed: true },
	]);
	expect(await (await call("GET", "/api/v1/health")).json()).toMatchObject({
		alerts: 0,
	});

	const missing = await call("POST", "/api/v1/phones/NOPE/provision");
	expect(missing.status).toBe(409);
	expect(await missing.json()).toEqual({ error: "phone NOPE is not online" });
});

test("set-up and check refuse requests another site could make", async () => {
	const { call, phone } = await setup();
	phone.set("global", "wifi_on", "1");
	const path = `/api/v1/phones/${B30}/provision`;
	const refused: Record<string, string>[] = [
		{},
		{ "content-type": "application/x-www-form-urlencoded" },
		{ "content-type": "text/plain" },
		{ "content-type": "application/json", "sec-fetch-site": "cross-site" },
		{ "x-spareline": "1", "sec-fetch-site": "cross-site" },
	];
	for (const headers of refused) {
		const response = await call("POST", path, headers);
		expect([headers, response.status]).toEqual([headers, 403]);
	}
	const check = await call("POST", `/api/v1/phones/${B30}/check`, {});
	expect(check.status).toBe(403);
	expect(phone.applied).toEqual([]);

	expect(
		(
			await call("POST", path, {
				"x-spareline": "1",
				"sec-fetch-site": "same-origin",
			})
		).status,
	).toBe(200);
	expect(phone.applied).toEqual(["svc wifi disable"]);
});

test("without a policy the status has no profile and set-up answers 409", async () => {
	const { call } = await setup({ policy: null });
	const view = (await (
		await call("GET", "/api/v1/status")
	).json()) as StatusView;
	expect(view.profile).toBeNull();
	expect(view.dataThresholdBytes).toBeNull();
	expect(view.phones[0]?.policy?.check ?? null).toBeNull();
	for (const action of ["provision", "check"]) {
		const response = await call("POST", `/api/v1/phones/${B30}/${action}`);
		expect(response.status).toBe(409);
		expect(await response.json()).toEqual({
			error:
				"no policy profile is loaded (set POLICY_PROFILE to use phone rules)",
		});
	}
});
