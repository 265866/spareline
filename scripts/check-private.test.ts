import { expect, test } from "bun:test";
import { scan } from "./check-private";

const whats = (text: string, terms: string[] = []) =>
	scan("file", text, terms).map((finding) => finding.what);

test("fictional numbers, fake identifiers, and example emails pass", () => {
	expect(
		whats(
			[
				"+15125550101 (512) 555-0199",
				"imei 350000000000006 490154203237518",
				"iccid 8901555000000000001",
				"owner@example.com 1+265866@users.noreply.github.com",
				"127.0.0.1:5037 192.0.2.10",
				"1792631703000",
			].join("\n"),
		),
	).toEqual([]);
});

test("real-looking identifiers are reported", () => {
	expect(
		whats(
			[
				"+15125550200",
				"(206) 555-0200",
				"imei 356938035643809",
				"iccid 8901260000000000007",
				"someone@gmail.com",
				"phone.tailabc123.ts.net",
				"192.168.1.20 100.101.102.103",
			].join("\n"),
		),
	).toEqual([
		"phone number",
		"phone number",
		"IMEI",
		"ICCID",
		"email",
		"tailnet host",
		"private IP",
		"private IP",
	]);
});

test("denylisted terms match case-insensitively, also inside adb hex", () => {
	const hex = Buffer.from("host:transport:SECRET123").toString("hex");
	expect(whats(`{ "hex": "${hex}" }`, ["secret123"])).toEqual([
		"denylisted term",
	]);
});

// A UTF-16 string as dumpsys prints it in a Parcel: an ASCII column per 16
// bytes, a dot for each zero byte.
function parcel(value: string): string {
	const ascii = `........${[...value].map((char) => `${char}.`).join("")}`;
	const lines: string[] = [];
	for (let i = 0; i < ascii.length; i += 16)
		lines.push(
			`0x${i.toString(16).padStart(8, "0")}: 00000000 '${ascii.slice(i, i + 16)}'`,
		);
	return `Result: Parcel(\t\n${lines.join("\n")})`;
}

test("identifiers inside Parcel dumps and hex(...) columns are found", () => {
	const hex = (text: string) => Buffer.from(text).toString("hex").toUpperCase();
	expect(whats(parcel("356938035643809"))).toEqual(["IMEI"]);
	expect(whats(parcel("8901260000000000007"))).toEqual(["ICCID"]);
	expect(whats(parcel("350000000000006"))).toEqual([]);
	expect(
		whats(`Row: 0 _id=4, hex(body)=${hex("call (206) 555-0200")}`),
	).toEqual(["phone number"]);
	expect(
		whats(`Row: 0 _id=4, hex(body)=${hex("is the couch still for sale?")}`),
	).toEqual([]);
});

test("IMSIs are found unless all but the network code is zero", () => {
	expect(whats("imsi=310260123456789")).toEqual(["IMSI"]);
	expect(whats("subscriberId=31026012345678")).toEqual(["IMSI"]);
	expect(whats("subscriberId=310260000000000")).toEqual([]);
	expect(whats("imsi=310240001[****]")).toEqual([]);
});
