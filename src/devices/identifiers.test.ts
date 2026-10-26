import { expect, test } from "bun:test";
import { isIccid, isImei, subInfoCodes, toE164 } from "./identifiers";

test("IMEIs need 15 digits and a valid Luhn check digit", () => {
	// The example IMEI from 3GPP TS 23.003 and its check digit.
	expect(isImei("490154203237518")).toBe(true);
	expect(isImei("490154203237517")).toBe(false);
	expect(isImei("49015420323751")).toBe(false);
	expect(isImei("")).toBe(false);
});

test("ICCIDs start with 89 and have 19 or 20 digits", () => {
	expect(isIccid("8901555000000000001")).toBe(true);
	expect(isIccid("89015550000000000012")).toBe(true);
	expect(isIccid("8901260123456789F")).toBe(false);
	expect(isIccid("1234567890123456789")).toBe(false);
});

test("E.164 normalization for North American SIMs", () => {
	expect(toE164("15125550101", "us")).toBe("+15125550101");
	expect(toE164("5125550101", "us")).toBe("+15125550101");
	expect(toE164("(512) 555-0101", "US")).toBe("+15125550101");
	expect(toE164("+44 20 7946 0000", "us")).toBe("+442079460000");
	expect(toE164("5125550101", null)).toBeNull();
	expect(toE164("5125550101", "gb")).toBeNull();
	expect(toE164("not a number", "us")).toBeNull();
});

test("E.164 rejects a leading 0 country code and over-long numbers", () => {
	expect(toE164("+0123456789", "us")).toBeNull();
	expect(toE164("+1234567890123456", "us")).toBeNull();
	expect(toE164("+123456789012345", "us")).toBe("+123456789012345");
});

test("codes are looked up by SDK and verified by fingerprint", () => {
	expect(
		subInfoCodes({
			sdk: 34,
			fingerprint:
				"NUU/S6702X1/S6702XA:14/UP1A.231005.007/1726207303:user/release-keys",
		}),
	).toEqual({
		codes: { imeiForSlot: 4, iccidForSub: 14, numberForSub: 16 },
		verified: true,
	});
	expect(subInfoCodes({ sdk: 34, fingerprint: "other" })).toMatchObject({
		verified: false,
	});
	expect(subInfoCodes({ sdk: 35, fingerprint: null }).codes).toBeNull();
});
