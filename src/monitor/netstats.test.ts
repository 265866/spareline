import { beforeAll, expect, test } from "bun:test";
import { withMeteredTraffic } from "../../test/monitor-phone";
import { fixture } from "../../test/replay-shell";
import { splitSections } from "../devices/parse";
import { internetUsage, PHONE_UID, parseNetstats } from "./netstats";

let netstats: string;
let now: number;
beforeAll(async () => {
	const sections = splitSections(
		await fixture("nuu-s6702x", "policy-read.txt"),
	);
	netstats = sections.get("netstats") ?? "";
	now = Number(sections.get("clock"));
});

test("the B30's netstats: only IMS traffic, on unmetered mobile networks", () => {
	const entries = parseNetstats(netstats);
	const xt = entries.filter((entry) => entry.section === "xt");
	expect(xt.length).toBeGreaterThan(0);
	for (const entry of xt)
		expect(entry).toMatchObject({
			mobile: true,
			metered: false,
			subId: 1,
			uid: -1,
			bucketSeconds: 3600,
		});
	expect(
		entries.filter((entry) => entry.section === "uid").map((e) => e.uid),
	).toContain(0);
	expect(internetUsage(entries, now)).toEqual([]);
});

// Mobile data stays off on the real phone, so internet traffic is a
// stand-in: rows in the recorded per-uid format (see withMeteredTraffic).
test("internet bytes over 24 h are summed per SIM, with the apps that used them", () => {
	let text = withMeteredTraffic(netstats, { bytes: 600_000, now });
	text = withMeteredTraffic(text, { bytes: 50_000, now, uid: 10190 });
	const [usage] = internetUsage(parseNetstats(text), now);
	expect(usage).toEqual({
		subId: 1,
		rx: 650_000,
		tx: 0,
		uids: [
			{ uid: 10101, bytes: 600_000 },
			{ uid: 10190, bytes: 50_000 },
		],
	});
});

test("the phone process (MMS, IMS, video calls) and tagged rows do not count", () => {
	let text = withMeteredTraffic(netstats, {
		bytes: 9_000_000,
		now,
		uid: PHONE_UID,
	});
	text = text.replace(
		"InterfaceMapUpdater:",
		[
			"  ident=[{type=0, ratType=13, subscriberId=310260..., metered=true, defaultNetwork=true, oemManaged=OEM_NONE, subId=1}] uid=10101 set=DEFAULT tag=0xffffff01",
			"    NetworkStatsHistory: bucketDuration=7200",
			`      st=${now - 3600} rb=900000 rp=400 tb=0 tp=0 op=0`,
			"InterfaceMapUpdater:",
		].join("\n"),
	);
	expect(internetUsage(parseNetstats(text), now)).toEqual([]);
});

test("buckets that ended before the window do not count", () => {
	const entries = parseNetstats(
		withMeteredTraffic(netstats, { bytes: 700_000, now }),
	);
	// The row's bucket starts an hour before `now` and lasts two hours.
	expect(internetUsage(entries, now + 3600 + 10, 5)).toEqual([]);
	expect(internetUsage(entries, now + 3600 - 10, 600)[0]?.rx).toBe(700_000);
});

test("Wi-Fi rows and unparseable text count nothing", () => {
	const wifi = withMeteredTraffic(netstats, { bytes: 5_000_000, now }).replace(
		"type=0, ratType=13, subscriberId=310260..., metered=true",
		"type=1, ratType=13, subscriberId=310260..., metered=true",
	);
	expect(internetUsage(parseNetstats(wifi), now)).toEqual([]);
	expect(parseNetstats("")).toEqual([]);
	expect(parseNetstats("Can't find service: netstats")).toEqual([]);
});
