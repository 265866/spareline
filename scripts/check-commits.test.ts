import { expect, test } from "bun:test";
import { conventional } from "./check-commits";

test("conventional subjects pass", () => {
	for (const subject of [
		"feat(sms): store texts by number",
		"fix: wake the phone before streaming",
		"refactor(api)!: rename the status route",
		"ci: check commit subjects",
	])
		expect(conventional(subject)).toBe(true);
});

test("other subjects fail", () => {
	for (const subject of [
		"Set the radio log buffer automatically (#9)",
		"feat:missing space",
		"feature(sms): unknown type",
		"feat(SMS): uppercase scope",
		"fix: ",
	])
		expect(conventional(subject)).toBe(false);
});
