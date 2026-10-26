import { expect, test } from "bun:test";
import { fixture, ReplayShell } from "../../test/replay-shell";
import {
	HEALTH_SCRIPT,
	IDENTITY_SCRIPT,
	probeHealth,
	probeIdentity,
} from "./probe";

const B30 = "nuu-s6702x";
const SERIAL = "SPARELINE00001";

async function b30Shell(): Promise<ReplayShell> {
	return new ReplayShell()
		.answer(IDENTITY_SCRIPT, await fixture(B30, "identity.txt"))
		.answerPrefix(
			"printf '\\n@@sl:imei:0\\n'",
			await fixture(B30, "subscriber.txt"),
		)
		.answer(HEALTH_SCRIPT, await fixture(B30, "health.txt"));
}

test("identity of the B30 with one SIM in slot 0", async () => {
	const shell = await b30Shell();
	const identity = await probeIdentity(shell, SERIAL);
	expect(identity).toEqual({
		manufacturer: "NUU",
		model: "S6702X",
		androidVersion: "14",
		sdk: 34,
		fingerprint:
			"NUU/S6702X1/S6702XA:14/UP1A.231005.007/1726207303:user/release-keys",
		securityPatch: "2024-09-05",
		imeis: ["350000000000006", "350000000000014"],
		slots: [
			{ slot: 0, state: "present", simState: "LOADED" },
			{ slot: 1, state: "empty", simState: "ABSENT" },
		],
		sims: [
			{
				slot: 0,
				subId: 1,
				iccid: "8901555000000000001",
				iccidPrefix: "890155500",
				carrier: "T-Mobile",
				countryIso: "us",
				mcc: "310",
				mnc: "240",
				number: "+15125550101",
			},
		],
		defaultDataSubId: 1,
		defaultSmsSubId: 1,
		subInfoVerified: true,
		warnings: [],
	});
	// Two shell calls: props + isub, then every subscriber getter at once.
	expect(shell.calls).toHaveLength(2);
	expect(shell.calls[1]).toContain(
		"service call iphonesubinfo 4 i32 1 s16 com.android.shell s16 null",
	);
	expect(shell.calls[1]).toContain("service call iphonesubinfo 14 i32 1 ");
	expect(shell.calls[1]).toContain("service call iphonesubinfo 16 i32 1 ");
});

test("an SDK without known iphonesubinfo codes reads no identifiers", async () => {
	const identityText = (await fixture(B30, "identity.txt"))
		.replace("ro.build.version.sdk=34", "ro.build.version.sdk=36")
		.replace("ro.build.version.release=14", "ro.build.version.release=16");
	const shell = new ReplayShell().answer(IDENTITY_SCRIPT, identityText);
	const identity = await probeIdentity(shell, SERIAL);
	expect(identity.androidVersion).toBe("16");
	expect(identity.imeis).toEqual([null, null]);
	expect(identity.sims.map((sim) => [sim.slot, sim.iccid, sim.number])).toEqual(
		[[0, null, null]],
	);
	expect(identity.warnings[0]).toContain(
		"no iphonesubinfo codes for Android SDK 36",
	);
	expect(shell.calls).toHaveLength(1);
});

test("another SDK 34 build uses the codes but says they are unverified", async () => {
	const shell = await b30Shell();
	shell.answer(
		IDENTITY_SCRIPT,
		(await fixture(B30, "identity.txt")).replace(
			/ro\.build\.fingerprint=.*/,
			"ro.build.fingerprint=OTHER/x:14/y",
		),
	);
	const identity = await probeIdentity(shell, SERIAL);
	expect(identity.subInfoVerified).toBe(false);
	expect(identity.imeis[0]).toBe("350000000000006");
	expect(identity.warnings).toEqual([
		"iphonesubinfo codes for SDK 34 are unverified on this build",
	]);
});

test("a getter returning something that is not an IMEI is dropped with a warning", async () => {
	const subscriber = (await fixture(B30, "subscriber.txt")).replace(
		/@@sl:imei:1\n[\s\S]*?'\)/,
		"@@sl:imei:1\nResult: Parcel(\t00000000 00000002 00320031   '....1.2.')",
	);
	const shell = new ReplayShell()
		.answer(IDENTITY_SCRIPT, await fixture(B30, "identity.txt"))
		.answerPrefix("printf '\\n@@sl:imei:0\\n'", subscriber);
	const identity = await probeIdentity(shell, SERIAL);
	expect(identity.imeis).toEqual(["350000000000006", null]);
	expect(identity.warnings).toEqual([
		"IMEI of slot 1: unexpected value shape, ignored",
	]);
});

test("a SIM still loading is unknown, not removed", async () => {
	const identityText = (await fixture(B30, "identity.txt"))
		.replace("mSimState[0]=LOADED", "mSimState[0]=NOT_READY")
		.replace("gsm.sim.state=LOADED,ABSENT", "gsm.sim.state=NOT_READY,ABSENT");
	const shell = (await b30Shell()).answer(IDENTITY_SCRIPT, identityText);
	const identity = await probeIdentity(shell, SERIAL);
	expect(identity.slots.map((slot) => slot.state)).toEqual([
		"unknown",
		"empty",
	]);
	expect(identity.sims).toEqual([]);
});

test("health", async () => {
	const health = await probeHealth(await b30Shell(), SERIAL);
	expect(health.battery?.level).toBe(85);
	expect(health.battery?.temperatureDeciC).toBe(221);
	expect(health.simStates).toEqual(["LOADED", "ABSENT"]);
	expect(
		health.radios.map((radio) => [radio.slot, radio.voice, radio.signalLevel]),
	).toEqual([
		[0, "out_of_service", 1],
		[1, "power_off", 0],
	]);
});
