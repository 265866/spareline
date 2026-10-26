// IPhoneSubInfo binder transaction codes for `service call iphonesubinfo`.
// The codes follow the method order in IPhoneSubInfo.aidl, which changes
// between Android releases and can change in OEM builds, so they are looked
// up here and nowhere else. A code is used only when this table names it; any
// value it returns is still validated (IMEI checksum, ICCID shape), so a wrong
// code yields "unknown" rather than a wrong identifier.
export type SubInfoCodes = {
	// getDeviceIdForPhone(int phoneId, String callingPackage, String featureId)
	imeiForSlot: number;
	// getIccSerialNumberForSubscriber(int subId, String callingPackage, String featureId)
	iccidForSub: number;
	// getLine1NumberForSubscriber(int subId, String callingPackage, String featureId)
	numberForSub: number;
};

type Entry = {
	sdk: number;
	codes: SubInfoCodes;
	// Builds on which every code was checked on a real phone. Other builds of
	// the same SDK use the codes unverified.
	verifiedFingerprints: string[];
};

const TABLE: Entry[] = [
	{
		sdk: 34,
		codes: { imeiForSlot: 4, iccidForSub: 14, numberForSub: 16 },
		verifiedFingerprints: [
			// NUU B30 (S6702X), 2026-10-21; the number was checked against the SIM.
			"NUU/S6702X1/S6702XA:14/UP1A.231005.007/1726207303:user/release-keys",
		],
	},
];

type SubInfoLookup =
	| { codes: SubInfoCodes; verified: boolean }
	| { codes: null; reason: string };

export function subInfoCodes(build: {
	sdk: number | null;
	fingerprint: string | null;
}): SubInfoLookup {
	const entry = TABLE.find((candidate) => candidate.sdk === build.sdk);
	if (!entry)
		return {
			codes: null,
			reason: `no iphonesubinfo codes for Android SDK ${build.sdk ?? "unknown"}; add them to src/devices/identifiers.ts`,
		};
	return {
		codes: entry.codes,
		verified:
			build.fingerprint !== null &&
			entry.verifiedFingerprints.includes(build.fingerprint),
	};
}

// The shell command for one IPhoneSubInfo getter, called as the shell user.
export function subInfoCall(code: number, index: number): string {
	return `service call iphonesubinfo ${code} i32 ${index} s16 com.android.shell s16 null`;
}

export function isImei(value: string): boolean {
	return /^\d{15}$/.test(value) && luhnValid(value);
}

// ICCIDs are 19 or 20 digits starting with the telecom industry prefix 89
// (ITU-T E.118); some carriers append a hex check character.
export function isIccid(value: string): boolean {
	return /^89\d{16,19}[0-9A-Fa-f]?$/.test(value);
}

function luhnValid(digits: string): boolean {
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

// Country calling codes for the SIM country ISO codes the app recognizes.
// North American Numbering Plan countries share +1.
const CALLING_CODE: Record<string, string> = { us: "1", ca: "1", pr: "1" };

// Normalizes a phone number read from a SIM or typed by the owner to E.164
// (+15125550101). Returns null when it cannot tell the country code.
export function toE164(raw: string, countryIso: string | null): string | null {
	const trimmed = raw.trim();
	const digits = trimmed.replace(/[\s().-]/g, "");
	// E.164: a country code never starts with 0, and the whole number has at
	// most 15 digits.
	if (/^\+[1-9]\d{7,14}$/.test(digits)) return digits;
	if (digits.startsWith("+")) return null;
	if (!/^\d+$/.test(digits)) return null;
	const code = countryIso ? CALLING_CODE[countryIso.toLowerCase()] : undefined;
	if (code === "1") {
		if (/^1\d{10}$/.test(digits)) return `+${digits}`;
		if (/^\d{10}$/.test(digits)) return `+1${digits}`;
	}
	return null;
}
