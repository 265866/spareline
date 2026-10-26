import { z } from "zod";

// The phone policy's part of the HTTP contract (src/monitor/).

const iso = z.string().describe("ISO 8601 UTC time");

const Severity = z.enum(["critical", "warning", "info"]);

export const RuleResult = z.object({
	id: z.string(),
	describe: z.string(),
	want: z.string(),
	severity: Severity,
	autoFix: z
		.boolean()
		.describe(
			"Applied automatically by the drift checker on sight: the radio and data rules, and the radio log buffer",
		),
	instruction: z
		.string()
		.nullable()
		.describe(
			"When the rule fails: what to do on the phone if adb cannot set it, or what happens next if the automatic fix did not take",
		),
	fixable: z.boolean().describe("Provision can set it over adb"),
	ok: z.boolean(),
	observed: z.string(),
});

const SimUsage = z.object({
	subId: z.number().int(),
	slot: z.number().int().nullable(),
	rx: z.number().int(),
	tx: z.number().int(),
	over: z.boolean().describe("Above the data threshold"),
	uids: z.array(z.object({ uid: z.number().int(), bytes: z.number().int() })),
});

export const CheckReport = z.object({
	at: iso,
	reason: z.enum(["connect", "interval", "manual", "sim"]),
	readMs: z
		.number()
		.int()
		.describe("Duration of the one batched shell read; 0 after Provision"),
	results: z.array(RuleResult),
	usage: z
		.array(SimUsage)
		.describe("Non-IMS mobile bytes per SIM over the trailing 24 h"),
	reverted: z.array(
		z.object({
			rule: z.string(),
			before: z.string(),
			after: z.string(),
			fixed: z.boolean(),
		}),
	),
	error: z.string().nullable(),
});

export const ProvisionReport = z.object({
	at: iso,
	durationMs: z.number().int(),
	steps: z
		.array(
			z.object({
				rule: z.string(),
				commands: z.array(z.string()),
				failed: z.array(z.object({ command: z.string(), output: z.string() })),
			}),
		)
		.describe("What it changed; empty when the phone already complied"),
	results: z.array(RuleResult),
	ok: z.boolean().describe("Every rule passes afterwards"),
	error: z.string().nullable(),
});

export const PhonePolicy = z.object({
	busy: z.enum(["check", "provision", "sim"]).nullable(),
	check: CheckReport.nullable(),
	provision: ProvisionReport.nullable(),
	dataAlarm: z
		.boolean()
		.describe("A SIM used mobile data above the threshold in 24 h"),
});

export const Alert = z.object({
	key: z.string(),
	kind: z.enum([
		"offline",
		"no_permissions",
		"out_of_service",
		"battery_hot",
		"policy",
		"drift_unfixed",
		"data_threshold",
		"check_failed",
		"sim_unsecured",
	]),
	serial: z.string(),
	severity: Severity,
	message: z.string(),
	since: iso,
});

export type RuleResultView = z.infer<typeof RuleResult>;
export type CheckReportView = z.infer<typeof CheckReport>;
export type AlertView = z.infer<typeof Alert>;
export type ProvisionReportView = z.infer<typeof ProvisionReport>;
