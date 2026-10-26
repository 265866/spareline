// `dumpsys netstats --full --uid` on Android 14, and the data watch built on
// it. Recorded output is in test/fixtures/device/<phone>/policy-read.txt.
//
//   Xt stats:
//     Complete history:
//     ident=[{type=0, ratType=13, subscriberId=310240..., metered=false, ..., subId=1}] uid=-1 set=ALL tag=0x0
//       NetworkStatsHistory: bucketDuration=3600
//         st=1792630800 rb=832 rp=8 tb=0 tp=0 op=0
//   UID stats:
//     ...same shape, one entry per uid, counter set, and tag

export type NetBucket = { start: number; rx: number; tx: number };

export type NetEntry = {
	section: "xt" | "uid";
	// A cellular network (NetworkTemplate type 0).
	mobile: boolean;
	metered: boolean;
	subId: number | null;
	uid: number;
	tag: string;
	bucketSeconds: number;
	buckets: NetBucket[];
};

const SECTIONS: Record<string, NetEntry["section"]> = {
	"Xt stats:": "xt",
	"UID stats:": "uid",
};

export function parseNetstats(text: string): NetEntry[] {
	const entries: NetEntry[] = [];
	let section: NetEntry["section"] | null = null;
	let current: NetEntry | null = null;
	for (const line of text.split(/\r?\n/)) {
		if (/^\S/.test(line)) {
			section = SECTIONS[line.trim()] ?? null;
			current = null;
			continue;
		}
		if (!section) continue;
		const ident = /^\s+ident=\[(.*)\] uid=(-?\d+) set=(\S+) tag=(\S+)/.exec(
			line,
		);
		if (ident) {
			// An ident set lists one or more identities of one network.
			const identities = [...(ident[1] ?? "").matchAll(/\{([^}]*)\}/g)].map(
				(match) => match[1] ?? "",
			);
			const field = (name: string) =>
				identities
					.map((body) => new RegExp(`\\b${name}=([^,\\s]+)`).exec(body)?.[1])
					.find((value) => value !== undefined);
			const subId = field("subId");
			current = {
				section,
				mobile: identities.some((body) => /\btype=0\b/.test(body)),
				metered: identities.some((body) => /\bmetered=true\b/.test(body)),
				subId: subId && /^-?\d+$/.test(subId) ? Number(subId) : null,
				uid: Number(ident[2]),
				tag: ident[4] ?? "",
				bucketSeconds: 0,
				buckets: [],
			};
			entries.push(current);
			continue;
		}
		if (!current) continue;
		const duration = /bucketDuration=(\d+)/.exec(line);
		if (duration) {
			current.bucketSeconds = Number(duration[1]);
			continue;
		}
		const bucket = /^\s+st=(\d+) rb=(\d+) rp=\d+ tb=(\d+)/.exec(line);
		if (bucket)
			current.buckets.push({
				start: Number(bucket[1]),
				rx: Number(bucket[2]),
				tx: Number(bucket[3]),
			});
	}
	return entries;
}

export type SimUsage = {
	subId: number;
	rx: number;
	tx: number;
	// The apps that used it, most first, from the per-uid stats.
	uids: { uid: number; bytes: number }[];
};

// The phone process (Android's PHONE_UID). MmsService, which downloads and
// sends MMS for Messages, and the IMS and video-call stacks run in it.
export const PHONE_UID = 1001;

// Mobile bytes an app could have used over the internet, per SIM, over the
// trailing window by the phone's clock. Counted from the per-uid stats on
// metered mobile networks, leaving out the phone process: the MMS APN is
// metered by default and MMS must keep working with data off, and the B30
// lists a metered vt_data01 (video calling) interface with data off
// (recorded). IMS itself runs on an unmetered APN (recorded: every mobile
// network with traffic on the B30 is metered=false). Inferred from AOSP and
// the recording; a live MMS test will confirm the MMS bytes land on uid
// 1001. A bucket that overlaps the window counts in full, which errs toward
// alerting.
export function internetUsage(
	entries: NetEntry[],
	nowSeconds: number,
	windowSeconds = 86_400,
): SimUsage[] {
	const since = nowSeconds - windowSeconds;
	const bySub = new Map<number, SimUsage>();
	for (const entry of entries) {
		// Tagged rows are subsets of the untagged ones.
		if (
			entry.section !== "uid" ||
			!entry.mobile ||
			!entry.metered ||
			entry.subId === null ||
			entry.tag !== "0x0" ||
			entry.uid === PHONE_UID
		)
			continue;
		let rx = 0;
		let tx = 0;
		for (const bucket of entry.buckets)
			if (bucket.start + entry.bucketSeconds > since) {
				rx += bucket.rx;
				tx += bucket.tx;
			}
		if (rx + tx === 0) continue;
		const usage = bySub.get(entry.subId) ?? {
			subId: entry.subId,
			rx: 0,
			tx: 0,
			uids: [],
		};
		usage.rx += rx;
		usage.tx += tx;
		const found = usage.uids.find((item) => item.uid === entry.uid);
		if (found) found.bytes += rx + tx;
		else usage.uids.push({ uid: entry.uid, bytes: rx + tx });
		bySub.set(entry.subId, usage);
	}
	for (const usage of bySub.values())
		usage.uids.sort((a, b) => b.bytes - a.bytes);
	return [...bySub.values()].sort((a, b) => a.subId - b.subId);
}
