// Fails when tracked files hold something that looks like a real identifier:
// a phone number outside the fictional 555-01xx range, an IMEI, ICCID, or
// IMSI that is not one of the fakes the fixture scripts write, a personal
// email, a tailnet host name, or a private IP address. It also reads the
// encodings the fixtures store text in: adb transcripts' hex, SMS provider
// rows' hex(...)= columns, and the ASCII column of Parcel dumps.
//
//   bun scripts/check-private.ts             # the working tree's tracked files
//   bun scripts/check-private.ts --history   # every blob and message in history
//
// .private-denylist (untracked, one term per line, # for comments) adds terms
// that no pattern can recognize, such as a phone's serial or a host name.
import { existsSync, readFileSync } from "node:fs";

// The test holds made-up identifiers that must be reported.
const SKIP = [
	/^bun\.lock$/,
	/^vendor\//,
	/^LICENSE$/,
	/\.bin$/,
	/^scripts\/check-private\.test\.ts$/,
];

// Fakes written by scripts/capture-fixtures.ts and the tests.
const FAKE_IMEI = /^(35000000000|4901542032375)/;
const FAKE_ICCID = /^8901555/;
const ALLOWED_EMAIL = /@(example\.(com|org|net)|users\.noreply\.github\.com)$/i;

type Finding = { where: string; what: string; match: string };

function luhn(digits: string): boolean {
	let sum = 0;
	for (let i = 0; i < digits.length; i++) {
		let digit = Number(digits[digits.length - 1 - i]);
		if (i % 2 === 1) {
			digit *= 2;
			if (digit > 9) digit -= 9;
		}
		sum += digit;
	}
	return sum % 10 === 0;
}

function denylist(): string[] {
	if (!existsSync(".private-denylist")) return [];
	return readFileSync(".private-denylist", "utf8")
		.split(/\r?\n/)
		.map((line) => line.trim())
		.filter((line) => line && !line.startsWith("#"));
}

// The text plus every encoded string in it, decoded.
function withDecoded(text: string): string {
	const decoded = [
		// adb transcripts (test/fixtures/adb/).
		...[...text.matchAll(/"hex": "([0-9a-f]+)"/g)].map((match) =>
			Buffer.from(match[1] ?? "", "hex").toString("latin1"),
		),
		// SMS and MMS provider rows: hex(body)=..., hex(sub)=...
		...[...text.matchAll(/\bhex\(\w+\)=([0-9A-Fa-f]+)/g)].map((match) =>
			Buffer.from(match[1] ?? "", "hex").toString("utf8"),
		),
		// Parcel dumps spell a UTF-16 string across lines of the ASCII column,
		// with a dot for each zero byte.
		...[...text.matchAll(/Parcel\(([\s\S]*?)'\)/g)].map((match) =>
			[...`${match[1]}'`.matchAll(/'([^'\n]*)'/g)]
				.map((column) => column[1] ?? "")
				.join("")
				.replaceAll(".", "")
				.trim(),
		),
	];
	return decoded.length ? `${text}\n${decoded.join("\n")}` : text;
}

export function scan(where: string, input: string, terms: string[]): Finding[] {
	const text = withDecoded(input);
	const found: Finding[] = [];
	const add = (what: string, match: string) =>
		found.push({ where, what, match });
	const lower = text.toLowerCase();
	for (const term of terms)
		if (lower.includes(term.toLowerCase())) add("denylisted term", term);
	// NANP numbers written with + or with separators; 555-0100 to 555-0199 are
	// reserved for fiction.
	for (const match of text.matchAll(
		/(?<![\d+])(?:\+1[\s.-]?|1[\s.-])?\(?([2-9]\d{2})\)?[\s.-]?([2-9]\d{2})[\s.-]?(\d{4})(?!\d)/g,
	)) {
		const [whole, , exchange, line] = match;
		const formatted = /[+()\s.-]/.test(whole);
		if (!formatted) continue;
		if (exchange === "555" && /^01\d\d$/.test(line ?? "")) continue;
		add("phone number", whole);
	}
	for (const match of text.matchAll(/(?<![\dA-Fa-f.])\d{15}(?![\dA-Fa-f.])/g))
		if (luhn(match[0]) && !FAKE_IMEI.test(match[0])) add("IMEI", match[0]);
	for (const match of text.matchAll(/(?<!\d)89\d{17,18}(?!\d)/g))
		if (!FAKE_ICCID.test(match[0])) add("ICCID", match[0]);
	// The fakes keep the network code (MCC and MNC) and zero the rest.
	for (const match of text.matchAll(
		/\b(?:imsi|subscriberId)=(\d{6})(\d{8,9})(?!\d)/gi,
	))
		if (/[1-9]/.test(match[2] ?? "")) add("IMSI", match[0]);
	for (const match of text.matchAll(/[\w.+-]+@[\w-]+(?:\.[\w-]+)+/g))
		if (!ALLOWED_EMAIL.test(match[0]) && /\.[a-z]{2,}$/i.test(match[0]))
			add("email", match[0]);
	for (const match of text.matchAll(/\b[\w-]+\.ts\.net\b/g))
		add("tailnet host", match[0]);
	for (const match of text.matchAll(
		/\b(?:10\.\d{1,3}|192\.168|172\.(?:1[6-9]|2\d|3[01])|100\.(?:6[4-9]|[7-9]\d|1[01]\d|12[0-7]))\.\d{1,3}\.\d{1,3}\b/g,
	))
		add("private IP", match[0]);
	return found;
}

async function git(...args: string[]): Promise<string> {
	const proc = Bun.spawn(["git", ...args], { stdout: "pipe" });
	const out = await new Response(proc.stdout).text();
	if ((await proc.exited) !== 0)
		throw new Error(`git ${args.join(" ")} failed`);
	return out;
}

if (import.meta.main) {
	const terms = denylist();
	const findings: Finding[] = [];
	const skipped = (path: string) => SKIP.some((pattern) => pattern.test(path));
	if (Bun.argv.includes("--history")) {
		// Identities are listed for a person to judge, not matched.
		const identities = await git(
			"log",
			"--all",
			"--format=author %an <%ae>, committer %cn <%ce>",
		);
		for (const identity of new Set(identities.split("\n").filter(Boolean)))
			console.log(identity);
		const seen = new Set<string>();
		for (const commit of (await git("rev-list", "--all")).split("\n")) {
			if (!commit) continue;
			const message = await git("log", "-1", "--format=%B", commit);
			findings.push(...scan(`commit ${commit.slice(0, 7)}`, message, terms));
			for (const line of (await git("ls-tree", "-r", commit)).split("\n")) {
				const match = /^\d+ blob ([0-9a-f]+)\t(.+)$/.exec(line);
				if (!match?.[1] || !match[2] || seen.has(match[1]) || skipped(match[2]))
					continue;
				seen.add(match[1]);
				const text = await git("cat-file", "blob", match[1]);
				findings.push(
					...scan(`${commit.slice(0, 7)}:${match[2]}`, text, terms),
				);
			}
		}
	} else {
		for (const path of (await git("ls-files")).split("\n")) {
			if (!path || skipped(path) || !existsSync(path)) continue;
			findings.push(...scan(path, readFileSync(path, "utf8"), terms));
		}
	}
	for (const finding of findings)
		console.error(`${finding.where}: ${finding.what}: ${finding.match}`);
	if (findings.length > 0) process.exit(1);
	console.log(
		`No private identifiers found${terms.length ? ` (${terms.length} denylisted terms checked)` : ""}.`,
	);
}
