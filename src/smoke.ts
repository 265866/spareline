// Read-only device smoke: proves this build can reach the adb server and read
// every online phone. deploy/podman/update.sh runs it inside a new image before
// switching to it.
//
//   bun run smoke            fails unless at least SMOKE_MIN_PHONES (default 1)
//                            phones are online and each one answers
//
// It only reads (adb version, device list, getprop); it never changes a phone.

import { z } from "zod";
import { AdbClient } from "./adb/client";
import { parseEnv } from "./env";

try {
	const config = parseEnv(process.env);
	const minimum = z.coerce
		.number()
		.int()
		.min(0)
		.safeParse(process.env.SMOKE_MIN_PHONES || 1);
	if (!minimum.success)
		throw new Error(
			`SMOKE_MIN_PHONES must be a whole number of phones, not "${process.env.SMOKE_MIN_PHONES}"`,
		);
	const minPhones = minimum.data;
	const adb = new AdbClient(config.ADB_SERVER);
	const where = `${config.ADB_SERVER.host}:${config.ADB_SERVER.port}`;
	const version = await adb.version({ timeoutMs: 5_000 });
	const devices = await adb.listDevices({ timeoutMs: 5_000 });
	const online = devices.filter((device) => device.state === "device");
	console.log(
		`adb server ${where}: protocol ${version}, ${devices.length} device(s), ${online.length} online`,
	);
	for (const device of devices.filter((entry) => entry.state !== "device"))
		console.log(`  ${device.serial}: ${device.rawState}`);
	let failed = 0;
	for (const device of online) {
		try {
			const result = await adb.shell(
				device.serial,
				"getprop ro.product.model; getprop ro.build.version.release",
				{
					timeoutMs: 10_000,
				},
			);
			const [model, release] = new TextDecoder()
				.decode(result.stdout)
				.trim()
				.split(/\r?\n/);
			if (result.exitCode !== 0 || !model)
				throw new Error(`exit ${result.exitCode}, model "${model ?? ""}"`);
			console.log(`  ${device.serial}: ${model}, Android ${release ?? "?"}`);
		} catch (error) {
			failed++;
			console.log(
				`  ${device.serial}: FAILED ${error instanceof Error ? error.message : String(error)}`,
			);
		}
	}
	if (failed > 0) throw new Error(`${failed} online phone(s) did not answer`);
	if (online.length < minPhones)
		throw new Error(`expected at least ${minPhones} online phone(s)`);
	console.log("smoke ok");
} catch (error) {
	console.error(
		`smoke failed: ${error instanceof Error ? error.message : String(error)}`,
	);
	process.exit(1);
}
