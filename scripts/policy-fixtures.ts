// The monitor's part of scripts/capture-fixtures.ts: the policy's batched read
// (settings, isub, netstats, ...) and a slice of the radio log, for
// src/monitor/ tests. capture-fixtures.ts applies its identifier replacements
// to these too; this adds what only these outputs carry.
import type { AdbClient } from "../src/adb/client";
import { READ_SCRIPT } from "../src/monitor/policy";

export async function capturePolicy(
	adb: AdbClient,
	serial: string,
): Promise<Record<string, string>> {
	const text = async (command: string) =>
		new TextDecoder().decode(
			(await adb.shell(serial, command, { timeoutMs: 60_000 })).stdout,
		);
	const read = await text(READ_SCRIPT);
	const radio = await text("logcat -b radio -d -v brief");
	return {
		"policy-read.txt": scrubRead(read),
		"radio-log.txt": scrubRadio(radio),
	};
}

// netstats prints the start of each network's IMSI and, for Wi-Fi, the
// network name.
function scrubRead(read: string): string {
	return read
		.replace(
			/subscriberId=(\d+)/g,
			(_, digits: string) =>
				`subscriberId=${"310260000000000".slice(0, digits.length)}`,
		)
		.replace(/wifiNetworkKey="[^"]*"/g, 'wifiNetworkKey="FakeWifi"')
		.replace(/wifiNetworkKey=[^,\s}]+/g, "wifiNetworkKey=FakeWifi");
}

const RIL_REQUEST =
	/^(\S\/(?:Mtk)?RILJ\s*\(\s*\d+\): \[(?:UNSL|\d+)\][<>] (?:[A-Z]+: )?[A-Z][A-Z0-9_]*)\b/;
const THERMAL = /^I\/AT\s*\(\s*\d+\): \[\d\] AT[<>] (?:AT)?\+ETHERMAL/;

// Only RIL request and unsolicited-response names, cut before any payload
// (which carries cell identities), and the modem's thermal polls: enough to
// test that the SIM log filter ignores the radio log's usual traffic.
function scrubRadio(radio: string): string {
	const ril: string[] = [];
	const at: string[] = [];
	for (const line of radio.split(/\r?\n/)) {
		const request = RIL_REQUEST.exec(line)?.[1];
		if (request) ril.push(`${request.trimEnd()} [PHONE0]`);
		else if (THERMAL.test(line)) at.push(line.replace(/tid:\d+/, "tid:1"));
	}
	return `${[...ril.slice(-80), ...at.slice(-20)].join("\n")}\n`;
}
