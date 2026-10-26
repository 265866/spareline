// Runs a policy profile's batched read against one phone and prints every
// rule's state and, for a radios-off profile, the data watch. Read-only: it
// never runs an enforce command.
//
//   ADB_SERVER=127.0.0.1:5037 bun scripts/policy-check.ts SERIAL PROFILE
import { AdbClient, parseEndpoint } from "../src/adb/client";
import { internetUsage, parseNetstats } from "../src/monitor/netstats";
import { evaluate, loadPolicy } from "../src/monitor/policy";

const [serial, profile] = Bun.argv.slice(2);
if (!serial || !profile) {
	console.error(
		"usage: bun scripts/policy-check.ts SERIAL PROFILE (such as profiles/no-data.json)",
	);
	process.exit(2);
}
const policy = loadPolicy(profile);
const adb = new AdbClient(
	parseEndpoint(process.env.ADB_SERVER ?? "127.0.0.1:5037"),
);
const started = performance.now();
const result = await adb.shell(serial, policy.readScript, {
	timeoutMs: 60_000,
});
const ms = Math.round(performance.now() - started);
const snapshot = policy.parse(new TextDecoder().decode(result.stdout));
console.log(
	`read: 1 shell call, ${ms} ms, ${result.stdout.length} bytes, exit ${result.exitCode}`,
);
for (const rule of evaluate(snapshot, { screenSession: false }, policy.rules))
	console.log(
		`${rule.ok ? "ok  " : "FAIL"}  ${rule.id.padEnd(22)} ${rule.observed}${rule.ok ? "" : `  (want ${rule.want})`}`,
	);
if (policy.radiosOff) {
	const now = snapshot.phoneClock ?? Math.floor(Date.now() / 1000);
	const usage = internetUsage(parseNetstats(snapshot.netstats), now);
	console.log(
		`data watch: ${usage.length === 0 ? "no internet mobile traffic in 24 h" : usage.map((sim) => `sub ${sim.subId}: ${sim.rx + sim.tx} bytes`).join(", ")}`,
	);
}
