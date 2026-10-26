import { expect, test } from "bun:test";
import { parseEnv } from "./env";

test("defaults bind loopback on 8740 and use the local adb server", () => {
	expect(parseEnv({})).toEqual({
		HOST: "127.0.0.1",
		PORT: 8740,
		ADB_SERVER: { host: "127.0.0.1", port: 5037 },
		DATA_DIR: "data",
		SITE: "local",
		HEALTH_INTERVAL_S: 60,
		WEB_DIR: "dist/web",
		POLICY_CHECK_INTERVAL_S: 300,
		DATA_THRESHOLD_BYTES: 1_000_000,
	});
});

test("values are parsed and invalid ones are named", () => {
	expect(
		parseEnv({ PORT: "9000", ADB_SERVER: "192.0.2.10:15037" }),
	).toMatchObject({
		PORT: 9000,
		ADB_SERVER: { host: "192.0.2.10", port: 15037 },
	});
	expect(() => parseEnv({ PORT: "http" })).toThrow("PORT");
	expect(() => parseEnv({ ADB_SERVER: "localhost" })).toThrow("ADB_SERVER");
	expect(() => parseEnv({ HEALTH_INTERVAL_S: "1" })).toThrow(
		"HEALTH_INTERVAL_S",
	);
	expect(() => parseEnv({ DATA_THRESHOLD_BYTES: "-1" })).toThrow(
		"DATA_THRESHOLD_BYTES",
	);
});
