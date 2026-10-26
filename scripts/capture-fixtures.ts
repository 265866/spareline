// Records a phone's probe and SMS provider output as test fixtures, with its
// identifiers and texts replaced by obviously fake ones.
//
//   ADB_SERVER=127.0.0.1:5037 bun scripts/capture-fixtures.ts SERIAL test/fixtures/device/NAME
//
// It runs the same scripts the app runs (src/devices/probe.ts), so fixtures
// change when the probes do. IMEIs, ICCIDs (full and dumpsys's masked
// prefixes), IMSI prefixes, phone numbers, and cell ids are rewritten with
// same-length fakes, Parcel hex included, and the script refuses to write if
// any real value is still present. SMS senders become 555-01xx numbers and
// bodies become fake text of the same row; MMS subjects are replaced too.
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { AdbClient, parseEndpoint } from "../src/adb/client";
import { subInfoCodes } from "../src/devices/identifiers";
import {
	parseIsub,
	parseParcelString,
	splitSections,
} from "../src/devices/parse";
import {
	HEALTH_SCRIPT,
	IDENTITY_SCRIPT,
	probeIdentity,
	subscriberScript,
} from "../src/devices/probe";
import { loadPolicy } from "../src/monitor/policy";
import {
	decodeHex,
	ISMS_TRIGGER_SCRIPT,
	parseContentRows,
} from "../src/sms/parse";
import { commands, MMS_COLUMNS, SMS_COLUMNS } from "../src/sms/provider";
import { capturePolicy } from "./policy-fixtures";

const [serial, outDir, profile = "profiles/nuu-b30-no-data.json"] =
	Bun.argv.slice(2);
if (!serial || !outDir) {
	console.error(
		"usage: bun scripts/capture-fixtures.ts SERIAL OUT_DIR [PROFILE]",
	);
	process.exit(2);
}
const adb = new AdbClient(
	parseEndpoint(process.env.ADB_SERVER ?? "127.0.0.1:5037"),
);
const text = (bytes: Uint8Array) => new TextDecoder().decode(bytes);

const identityRaw = text((await adb.shell(serial, IDENTITY_SCRIPT)).stdout);
const identity = await probeIdentity(adb, serial);
const codes = subInfoCodes(identity);
if (!codes.codes) throw new Error("this build has no iphonesubinfo codes");
const subscriberRaw = text(
	(
		await adb.shell(
			serial,
			subscriberScript(
				codes.codes,
				identity.imeis.map((_, slot) => slot),
				identity.sims.map((sim) => sim.subId),
			),
		)
	).stdout,
);
const healthRaw = text((await adb.shell(serial, HEALTH_SCRIPT)).stdout);
const shellText = async (command: string) =>
	text((await adb.shell(serial, command, { timeoutMs: 30_000 })).stdout);
const ismsRaw = await shellText(ISMS_TRIGGER_SCRIPT);
const smsRaw = await shellText(commands.smsAfter(0));
const smsKeysRaw = await shellText(commands.smsKeys());
const mmsRaw = await shellText(commands.mmsAfter(0));
const mmsKeysRaw = await shellText(commands.mmsKeys());

// Real values, read from the raw output rather than the normalized identity so
// every textual form is caught.
const replacements = new Map<string, string>();
// The serial is never rewritten, only refused: no probe prints it.
const secrets: string[] = [serial];
const sections = splitSections(subscriberRaw);
let imeiIndex = 0;
for (const [name, body] of sections) {
	const parsed = parseParcelString(body);
	if (parsed.kind !== "value" || !parsed.value) continue;
	const real = parsed.value;
	let fake: string;
	if (name.startsWith("imei:")) fake = fakeImei(imeiIndex++);
	else if (name.startsWith("iccid:")) fake = fakeDigits("8901555", real.length);
	else fake = fakeNumber(real);
	replacements.set(real, fake);
	secrets.push(real);
	if (name.startsWith("number:")) secrets.push(real.replace(/^\+?1/, ""));
}
for (const sim of identity.sims) {
	const prefix = sim.iccidPrefix;
	const full = sim.iccid;
	if (prefix && full)
		replacements.set(
			prefix,
			replacements.get(full)?.slice(0, prefix.length) ??
				fakeDigits("8901555", prefix.length),
		);
}

function sanitizeText(input: string): string {
	let out = input;
	// Longest first, so an ICCID is replaced before its own prefix.
	for (const [real, fake] of [...replacements].sort(
		(a, b) => b[0].length - a[0].length,
	))
		out = out.replaceAll(real, fake);
	out = out.replace(
		/imsi=(\d+)\[\*\*\*\*\]/g,
		(_, digits: string) => `imsi=${fakeDigits("310240", digits.length)}[****]`,
	);
	out = out.replace(
		/\b(mCi|mPci|mTac|mNci|mLac|mCid|mBsic|mPsc)=(\d+)/g,
		(_, key: string, digits: string) =>
			`${key}=${fakeDigits("1", digits.length)}`,
	);
	return out;
}

// Rewrites the hex words and ASCII column of every Parcel in place.
function sanitizeParcels(input: string): string {
	return splitAround(input)
		.map((part) => {
			if (!part.startsWith("Result: Parcel(")) return part;
			const parsed = parseParcelString(part);
			if (parsed.kind !== "value" || !parsed.value) return part;
			const fake = replacements.get(parsed.value);
			if (!fake) return part;
			return rewriteParcel(part, fake);
		})
		.join("");
}

// Splits text so that each "Result: Parcel(...')" block is its own part.
function splitAround(input: string): string[] {
	const parts: string[] = [];
	let rest = input;
	for (;;) {
		const start = rest.indexOf("Result: Parcel(");
		if (start < 0) break;
		const end = rest.indexOf("')", start);
		if (end < 0) break;
		parts.push(rest.slice(0, start), rest.slice(start, end + 2));
		rest = rest.slice(end + 2);
	}
	parts.push(rest);
	return parts;
}

function rewriteParcel(block: string, value: string): string {
	const bytes = new Uint8Array(8 + Math.ceil((value.length * 2) / 4) * 4);
	const view = new DataView(bytes.buffer);
	view.setInt32(0, 0, true);
	view.setInt32(4, value.length, true);
	for (let i = 0; i < value.length; i++)
		view.setUint16(8 + i * 2, value.charCodeAt(i), true);
	let word = 0;
	return block
		.split("\n")
		.map((line) => {
			const prefix =
				/^(Result: Parcel\(\s*|\s*0x[0-9a-f]+:\s*)/i.exec(line)?.[0] ?? "";
			const rest = line.slice(prefix.length);
			const words = /^(?:[0-9a-f]{8}(?: +|$))+/i.exec(rest)?.[0];
			if (!words) return line;
			const first = word;
			const hex = words.replace(/[0-9a-f]{8}/gi, () =>
				view
					.getUint32(4 * word++, true)
					.toString(16)
					.padStart(8, "0"),
			);
			const tail = rest.slice(words.trimEnd().length);
			const ascii = /'([^']*)'/.exec(tail);
			let newTail = tail;
			if (ascii?.[1] !== undefined) {
				const chars = [...bytes.subarray(first * 4, word * 4)]
					.map((b) => (b >= 0x20 && b < 0x7f ? String.fromCharCode(b) : "."))
					.join("");
				newTail = tail.replace(
					`'${ascii[1]}'`,
					`'${chars.padEnd(ascii[1].length, " ")}'`,
				);
			}
			return prefix + hex.trimEnd() + newTail;
		})
		.join("\n");
}

function fakeImei(index: number): string {
	const body = `35000000${String(index).padStart(6, "0")}`;
	return body + luhnDigit(body);
}

function luhnDigit(body: string): string {
	let sum = 0;
	for (let i = 0; i < body.length; i++) {
		let digit = Number(body[body.length - 1 - i]);
		if (i % 2 === 0) {
			digit *= 2;
			if (digit > 9) digit -= 9;
		}
		sum += digit;
	}
	return String((10 - (sum % 10)) % 10);
}

function fakeDigits(prefix: string, length: number): string {
	return `${(prefix + "0".repeat(length)).slice(0, length - 1)}1`;
}

function fakeNumber(real: string): string {
	const digits = "5125550101";
	if (real.startsWith("+1")) return `+1${digits}`;
	if (real.startsWith("1") && real.length === 11) return `1${digits}`;
	return digits.slice(-real.length).padStart(real.length, "5");
}

// Senders and bodies of the inbox, row by row. Real values join the secrets
// that must not survive.
const fakeSenders = new Map<string, string>();
function fakeSender(real: string): string {
	let fake = fakeSenders.get(real);
	if (!fake) {
		const n = String(fakeSenders.size).padStart(2, "0");
		// 555-0150 and up: fictional, and clear of the SIM's fake 555-0101.
		fake = /^\+1\d{10}$/.test(real)
			? `+15125550${150 + fakeSenders.size}`
			: /^\d+$/.test(real)
				? `55${n}`.padEnd(real.length, "5").slice(0, real.length)
				: `Sender${n}`;
		fakeSenders.set(real, fake);
		secrets.push(real, real.replace(/^\+?1/, ""));
	}
	return fake;
}
const hex = (value: string) =>
	Buffer.from(value, "utf8").toString("hex").toUpperCase();

function sanitizeRows(
	raw: string,
	columns: readonly string[],
	fakeText: (index: number) => string,
): string {
	const { rows, malformed } = parseContentRows(raw, columns);
	if (malformed.length > 0)
		throw new Error("a provider row did not parse; not writing");
	let index = 0;
	return raw
		.split("\n")
		.map((line) => {
			if (!line.startsWith("Row: ")) return line;
			const row = rows[index];
			if (!row) throw new Error("row count mismatch; not writing");
			let out = line;
			if (row.address) {
				out = out.replace(
					`address=${row.address},`,
					`address=${fakeSender(row.address)},`,
				);
			}
			for (const column of ["hex(body)", "hex(sub)"]) {
				const value = row[column];
				if (!value) continue;
				const decoded = decodeHex(value);
				if (decoded.length >= 6) secrets.push(decoded);
				secrets.push(value);
				out = out.replace(
					`${column}=${value}`,
					`${column}=${hex(fakeText(index))}`,
				);
			}
			index++;
			return out;
		})
		.join("\n");
}

const outputs: Record<string, string> = {
	"identity.txt": sanitizeText(identityRaw),
	"subscriber.txt": sanitizeText(sanitizeParcels(subscriberRaw)),
	"health.txt": sanitizeText(healthRaw),
	"isms.txt": sanitizeText(
		ismsRaw.replace(
			/x-message-id:-?\d+/g,
			(_, offset: number) => `x-message-id:${1000 + offset}`,
		),
	),
	// Fake bodies keep a comma, an accent, and a symbol so the hex decoding
	// and the row parser are exercised with more than ASCII.
	"sms-provider.txt": sanitizeText(
		sanitizeRows(
			smsRaw,
			SMS_COLUMNS,
			(i) => `Fake text ${i + 1}, is the café table still for sale? ✓`,
		),
	),
	"sms-keys.txt": smsKeysRaw,
	"mms-provider.txt": sanitizeText(
		sanitizeRows(mmsRaw, MMS_COLUMNS, (i) => `Fake subject ${i + 1}`),
	),
	"mms-keys.txt": mmsKeysRaw,
};

for (const [name, body] of Object.entries(
	await capturePolicy(adb, serial, loadPolicy(profile)),
))
	outputs[name] = sanitizeText(body);
for (const [name, body] of Object.entries(outputs))
	for (const secret of secrets)
		if (secret.length >= 6 && body.includes(secret))
			throw new Error(`${name} still contains a real identifier; not writing`);
const isub = parseIsub(
	splitSections(outputs["identity.txt"] ?? "").get("isub") ?? "",
);
for (const sub of isub.active)
	if (sub.iccidPrefix && !sub.iccidPrefix.startsWith("8901555"))
		throw new Error("the masked ICCID prefix was not replaced; not writing");

mkdirSync(outDir, { recursive: true });
for (const [name, body] of Object.entries(outputs))
	writeFileSync(join(outDir, name), body);
writeFileSync(
	join(outDir, "meta.json"),
	`${JSON.stringify({ model: identity.model, fingerprint: identity.fingerprint, capturedAt: new Date().toISOString(), note: "IMEIs, ICCIDs, IMSI prefixes, phone numbers, cell ids, SMS senders, SMS bodies, MMS subjects, and isms message ids are fake; everything else is verbatim." }, null, "\t")}\n`,
);
console.log(`wrote ${Object.keys(outputs).length} fixtures to ${outDir}`);
