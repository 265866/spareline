import { join, resolve } from "node:path";
import pkg from "../package.json";
import { AdbClient } from "./adb/client";
import { createApp } from "./api/app";
import { Bus } from "./bus";
import { openDb } from "./db/db";
import { DeviceManager } from "./devices/manager";
import { parseEnv } from "./env";
import { Inventory } from "./inventory/store";
import type { DeviceEventRow } from "./inventory/sync";
import { log } from "./log";
import { Monitor } from "./monitor/monitor";
import { loadPolicy, type Policy } from "./monitor/policy";
import { screenSessionActive } from "./monitor/screen-sessions";
import { loadServerJar } from "./screens/jar";
import { ScreenManager } from "./screens/manager";
import { SmsIngest } from "./sms/ingest";
import { type Message, MessageStore } from "./sms/store";

let config: ReturnType<typeof parseEnv>;
let policy: Policy | null;
try {
	config = parseEnv(process.env);
	policy = config.POLICY_PROFILE ? loadPolicy(config.POLICY_PROFILE) : null;
} catch (error) {
	console.error(error instanceof Error ? error.message : String(error));
	process.exit(1);
}

const version = process.env.SPARELINE_REVISION ?? pkg.version ?? "dev";
const db = openDb(join(config.DATA_DIR, "spareline.db"));
const events = new Bus<DeviceEventRow>();
const devices = new DeviceManager({
	adb: new AdbClient(config.ADB_SERVER),
	db,
	site: config.SITE,
	events,
	log,
	healthIntervalMs: config.HEALTH_INTERVAL_S * 1000,
});
const messages = new Bus<Message>();
const store = new MessageStore(db);
const attachmentsDir = join(config.DATA_DIR, "attachments");
const sms = new SmsIngest({
	devices,
	db,
	store,
	messages,
	attachmentsDir,
	log,
});
const screens = new ScreenManager({
	devices,
	db,
	jar: await loadServerJar(),
	log,
});
const monitor = new Monitor({
	devices,
	db,
	events,
	log,
	checkIntervalMs: config.POLICY_CHECK_INTERVAL_S * 1000,
	healthIntervalMs: config.HEALTH_INTERVAL_S * 1000,
	policy,
	isScreenSessionActive: screenSessionActive(() => screens.list()),
});
const app = createApp({
	inventory: new Inventory(db),
	devices,
	events,
	messages: { store, bus: messages, attachmentsDir },
	monitor,
	webDir: resolve(config.WEB_DIR),
	version,
	screens,
});

sms.start();
monitor.start();
devices.start();
app.listen({ hostname: config.HOST, port: config.PORT });
log("listening", {
	url: `http://${config.HOST}:${config.PORT}`,
	adb: `${config.ADB_SERVER.host}:${config.ADB_SERVER.port}`,
	data: resolve(config.DATA_DIR),
	policy: policy?.name ?? null,
	version,
});

let stopping = false;
for (const signal of ["SIGTERM", "SIGINT"] as const)
	process.on(signal, async () => {
		if (stopping) return;
		stopping = true;
		log("stopping", { signal });
		await screens.stop();
		await app.stop();
		await sms.stop();
		await monitor.stop();
		await devices.stop();
		db.close();
		process.exit(0);
	});
