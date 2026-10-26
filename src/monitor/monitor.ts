import type { Database } from "bun:sqlite";
import type { Bus } from "../bus";
import type { Devices } from "../devices/manager";
import { parseIsub, sectionMarker, splitSections } from "../devices/parse";
import {
	type DeviceEventRow,
	insertEvents,
	type NewEvent,
} from "../inventory/sync";
import type { Log } from "../log";
import { type Alert, HealthRecorder } from "./health";
import { internetUsage, parseNetstats, type SimUsage } from "./netstats";
import {
	type ActiveSub,
	type CheckContext,
	evaluate,
	MMS_ALWAYS_ALLOWED,
	type Policy,
	plan,
	presetDataOffCommands,
	type Rule,
	type RuleResult,
	radioHazard,
	type Snapshot,
} from "./policy";
import {
	diffSlots,
	type SimChange,
	type SimSignal,
	SimWatch,
	type SlotMap,
} from "./sims";

export type MonitorOptions = {
	devices: Devices;
	db: Database;
	events: Bus<DeviceEventRow>;
	log: Log;
	// The rules from the policy profile; null checks and enforces none.
	policy: Policy | null;
	checkIntervalMs?: number;
	healthIntervalMs?: number;
	simPollMs?: number;
	// True while a screen session streams from the phone (scrcpy holds
	// stay_on_while_plugged_in). The screens module supplies it.
	isScreenSessionActive?: (serial: string) => boolean;
	// Quiet time after a SIM signal before isub is re-read, so one swap's
	// burst of log lines costs one read.
	simDebounceMs?: number;
	now?: () => Date;
};

export type CheckReason = "connect" | "interval" | "manual" | "sim";

export type Revert = {
	rule: string;
	before: string;
	after: string;
	fixed: boolean;
};

export type SimDataUsage = SimUsage & { slot: number | null; over: boolean };

export type CheckReport = {
	at: string;
	reason: CheckReason;
	// The batched read: one shell call.
	readMs: number;
	results: RuleResult[];
	usage: SimDataUsage[];
	reverted: Revert[];
	error: string | null;
};

export type ProvisionStep = {
	rule: string;
	commands: string[];
	// Commands that exited non-zero, with their output.
	failed: { command: string; output: string }[];
};

export type ProvisionReport = {
	at: string;
	durationMs: number;
	steps: ProvisionStep[];
	results: RuleResult[];
	// Every rule passes afterwards.
	ok: boolean;
	error: string | null;
};

export type MonitorPhone = {
	serial: string;
	busy: "check" | "provision" | "sim" | null;
	check: CheckReport | null;
	provision: ProvisionReport | null;
	dataAlarm: boolean;
};

type Runtime = {
	serial: string;
	queue: Promise<unknown>;
	busy: MonitorPhone["busy"];
	check: CheckReport | null;
	provision: ProvisionReport | null;
	provisionLoaded: boolean;
	// Which sub is in each slot, as last seen; null until the first read
	// after the phone came online.
	slots: SlotMap | null;
	watch: SimWatch | null;
	simTimer: Timer | undefined;
	// Subs over the data threshold at the last check.
	alarmed: Set<number>;
	// The data-off run on a SIM signal, outside the queue, and whether a
	// signal arrived during it.
	dataOffRunning: Promise<void> | null;
	dataOffAgain: boolean;
	// Turning data off failed; cleared by a check that started after the
	// failure (seq orders them) and finds mobile data off.
	dataOffFailure: {
		at: string;
		seq: number;
		cause: DataOffCause;
		error: string;
	} | null;
};

// Why mobile data had to be turned off.
type DataOffCause = "sim" | "data_threshold";

export class MonitorError extends Error {}

// Looks after every online phone: health aggregates and alerts, and SIM
// hotplug for the inventory. With a policy it also runs the policy: a drift
// check on connect and every 5 minutes (radio and data drift reverted on
// sight, which is all a phone gets automatically), Provision on request, and,
// when the policy keeps radios off, the data watch and data-off on SIM
// changes. Work on one phone runs one step at a time, except the data-off
// that a SIM signal triggers.
export class Monitor {
	private readonly runtimes = new Map<string, Runtime>();
	private readonly health: HealthRecorder;
	private readonly unsubscribe: (() => void)[] = [];
	private current = new Map<string, Alert>();
	private checkTimer: Timer | undefined;
	private healthTimer: Timer | undefined;
	private running = false;
	// Orders data-off failures against the checks that may clear them.
	private sequence = 0;
	private readonly now: () => Date;
	readonly policy: Policy | null;

	constructor(private readonly options: MonitorOptions) {
		this.now = options.now ?? (() => new Date());
		this.health = new HealthRecorder(options.db);
		this.policy = options.policy;
	}

	// Internet mobile bytes per SIM per trailing 24 h before data is turned
	// off again and the phone goes red; null without a radios-off policy.
	get dataThresholdBytes(): number | null {
		return this.policy?.radiosOff?.dataThresholdBytes ?? null;
	}

	start(): void {
		if (this.running) return;
		this.running = true;
		const { devices } = this.options;
		this.unsubscribe.push(
			devices.on("connected", (serial) => this.connected(serial)),
			devices.on("disconnected", (serial) => this.disconnected(serial)),
		);
		for (const phone of devices.list())
			if (phone.status === "online") this.connected(phone.serial);
		const policy = this.policy;
		if (policy)
			this.checkTimer = setInterval(
				() => {
					for (const phone of devices.list())
						if (phone.status === "online") {
							const runtime = this.runtime(phone.serial);
							if (!runtime.busy)
								this.enqueue(runtime, "check", () =>
									this.runCheck(runtime, policy, "interval"),
								).catch((error) => this.unexpected(runtime, "interval", error));
						}
				},
				this.options.checkIntervalMs ?? 5 * 60_000,
			);
		this.healthTimer = setInterval(
			() => this.sampleHealth(),
			this.options.healthIntervalMs ?? 60_000,
		);
	}

	async stop(): Promise<void> {
		this.running = false;
		clearInterval(this.checkTimer);
		clearInterval(this.healthTimer);
		for (const off of this.unsubscribe.splice(0)) off();
		for (const runtime of this.runtimes.values()) {
			runtime.watch?.stop();
			clearTimeout(runtime.simTimer);
		}
		await this.settled();
		this.health.flush((serial) => this.mobileBytes(serial));
	}

	async settled(): Promise<void> {
		for (;;) {
			const queues = [...this.runtimes.values()].map((r) => r.queue);
			await Promise.allSettled(queues);
			if (
				[...this.runtimes.values()].every(
					(runtime, index) => runtime.queue === queues[index],
				)
			)
				return;
		}
	}

	phones(): MonitorPhone[] {
		return [...this.runtimes.values()].map((runtime) => this.view(runtime));
	}

	phone(serial: string): MonitorPhone | null {
		const runtime = this.runtimes.get(serial);
		return runtime ? this.view(runtime) : null;
	}

	alerts(): Alert[] {
		return [...this.current.values()];
	}

	// Runs the drift check now.
	async check(serial: string): Promise<CheckReport> {
		const policy = this.required();
		const runtime = this.online(serial);
		return this.enqueue(runtime, "check", () =>
			this.runCheck(runtime, policy, "manual"),
		);
	}

	// Sets the phone up to match the policy: applies every enforceable rule,
	// including removing apps and the default SMS app, then verifies. Only on
	// request: it removes apps and their data.
	async provision(serial: string): Promise<ProvisionReport> {
		const policy = this.required();
		const runtime = this.online(serial);
		return this.enqueue(runtime, "provision", () =>
			this.runProvision(runtime, policy),
		);
	}

	// Samples health now; for tests and the timer.
	sampleHealth(): void {
		this.health.sample(this.options.devices.list(), this.now(), (serial) =>
			this.mobileBytes(serial),
		);
		this.refreshAlerts();
	}

	private required(): Policy {
		if (!this.policy)
			throw new MonitorError(
				"no policy profile is loaded (set POLICY_PROFILE to use phone rules)",
			);
		return this.policy;
	}

	private online(serial: string): Runtime {
		if (this.options.devices.get(serial)?.status !== "online")
			throw new MonitorError(`phone ${serial} is not online`);
		return this.runtime(serial);
	}

	private runtime(serial: string): Runtime {
		let runtime = this.runtimes.get(serial);
		if (!runtime) {
			runtime = {
				serial,
				queue: Promise.resolve(),
				busy: null,
				check: null,
				provision: null,
				provisionLoaded: false,
				slots: null,
				watch: null,
				simTimer: undefined,
				alarmed: new Set(),
				dataOffRunning: null,
				dataOffAgain: false,
				dataOffFailure: null,
			};
			this.runtimes.set(serial, runtime);
		}
		return runtime;
	}

	private view(runtime: Runtime): MonitorPhone {
		if (!runtime.provisionLoaded) {
			runtime.provisionLoaded = true;
			runtime.provision ??= this.lastProvision(runtime.serial);
		}
		return {
			serial: runtime.serial,
			busy: runtime.busy,
			check: runtime.check,
			provision: runtime.provision,
			dataAlarm: runtime.alarmed.size > 0,
		};
	}

	private enqueue<T>(
		runtime: Runtime,
		busy: NonNullable<MonitorPhone["busy"]>,
		work: () => Promise<T>,
	): Promise<T> {
		const run = runtime.queue
			.catch(() => {})
			.then(async () => {
				runtime.busy = busy;
				try {
					return await work();
				} finally {
					runtime.busy = null;
				}
			});
		runtime.queue = run.catch(() => {});
		return run;
	}

	private connected(serial: string): void {
		if (!this.running) return;
		const runtime = this.runtime(serial);
		runtime.slots = null;
		runtime.watch?.stop();
		runtime.watch = new SimWatch({
			devices: this.options.devices,
			serial,
			log: this.options.log,
			pollMs: this.options.simPollMs ?? 10_000,
			retryMs: 30_000,
			onSignal: (reason) => this.simSignal(runtime, reason),
		});
		runtime.watch.start();
		const policy = this.policy;
		if (policy)
			this.enqueue(runtime, "check", () =>
				this.runCheck(runtime, policy, "connect"),
			).catch((error) => this.unexpected(runtime, "connect", error));
	}

	private disconnected(serial: string): void {
		const runtime = this.runtimes.get(serial);
		if (!runtime) return;
		runtime.watch?.stop();
		runtime.watch = null;
		runtime.slots = null;
		clearTimeout(runtime.simTimer);
		this.refreshAlerts();
	}

	private simSignal(runtime: Runtime, reason: SimSignal): void {
		// Data off at once, without waiting behind queued work: a new SIM's
		// data may already be coming up. Running beside a check is safe, since
		// both only turn data off.
		if (this.policy?.radiosOff) this.dataOffNow(runtime);
		clearTimeout(runtime.simTimer);
		runtime.simTimer = setTimeout(() => {
			if (!this.running || !runtime.watch) return;
			this.enqueue(runtime, "sim", () => this.simCheck(runtime, reason)).catch(
				(error) => this.unexpected(runtime, "sim", error),
			);
		}, this.options.simDebounceMs ?? 1_000);
	}

	// One data-off at a time per phone. A signal that arrives while one runs
	// gets its own run afterwards, since its SIM may have come up after the
	// running one's commands.
	private dataOffNow(runtime: Runtime): void {
		if (runtime.dataOffRunning) {
			runtime.dataOffAgain = true;
			return;
		}
		runtime.dataOffRunning = (async () => {
			do {
				runtime.dataOffAgain = false;
				try {
					await this.apply(runtime.serial, dataOffCommands([]));
				} catch (error) {
					this.dataOffFailed(runtime, "sim", error);
				}
			} while (runtime.dataOffAgain && this.running);
		})().finally(() => {
			runtime.dataOffRunning = null;
		});
	}

	private dataOffFailed(
		runtime: Runtime,
		cause: DataOffCause,
		error: unknown,
	): void {
		runtime.dataOffFailure = {
			at: this.now().toISOString(),
			seq: ++this.sequence,
			cause,
			error: errorText(error),
		};
		this.options.log("data-off-failed", {
			serial: runtime.serial,
			cause,
			error: errorText(error),
		});
		this.refreshAlerts();
	}

	// A rejection the step itself did not record; checks record their own
	// failures, so this is a bug or a failure outside them.
	private unexpected(runtime: Runtime, step: string, error: unknown): void {
		this.options.log("monitor-step-failed", {
			serial: runtime.serial,
			step,
			error: errorText(error),
		});
	}

	private context(serial: string): CheckContext {
		return {
			screenSession: this.options.isScreenSessionActive?.(serial) ?? false,
		};
	}

	private async read(
		serial: string,
		policy: Policy,
	): Promise<{ snapshot: Snapshot; ms: number }> {
		const started = performance.now();
		const result = await this.options.devices.shell(serial, policy.readScript, {
			timeoutMs: 60_000,
		});
		const ms = Math.round(performance.now() - started);
		const stdout = new TextDecoder().decode(result.stdout);
		const sections = splitSections(stdout);
		if (!sections.has(policy.lastSection))
			throw new Error(
				`policy read was cut short (exit ${result.exitCode}): ${new TextDecoder().decode(result.stderr).trim()}`,
			);
		assertIsub(sections.get("isub") ?? "");
		const snapshot = policy.parse(stdout);
		// Every namespace read has keys the policy needs; an empty one means
		// the settings tool failed, not that the settings are gone.
		for (const ns of policy.namespaces)
			if (snapshot.settings[ns].size === 0)
				throw new Error(`policy read returned no ${ns} settings`);
		return { snapshot, ms };
	}

	// Runs commands in one shell call after the radio guard has passed every
	// one of them.
	private async apply(
		serial: string,
		commands: string[],
	): Promise<{ command: string; exitCode: number | null; output: string }[]> {
		if (commands.length === 0) return [];
		for (const command of commands) {
			const hazard = radioHazard(command);
			if (hazard) throw new Error(`refusing to run "${command}" (${hazard})`);
		}
		let script = commands
			.map(
				(command, index) =>
					`${sectionMarker(`run:${index}`)}; ${command} 2>&1; echo "rc=$?"`,
			)
			.join("; ");
		// svc returns before the radio settles; give the setting a moment so
		// the re-check reads the result.
		if (commands.some((command) => command.startsWith("svc ")))
			script += "; sleep 1";
		this.options.log("policy-apply", { serial, commands });
		const result = await this.options.devices.shell(serial, script, {
			timeoutMs: 120_000,
		});
		const sections = splitSections(new TextDecoder().decode(result.stdout));
		return commands.map((command, index) => {
			const body = sections.get(`run:${index}`) ?? "";
			const rc = /(?:^|\n)rc=(\d+)\s*$/.exec(body);
			return {
				command,
				exitCode: rc ? Number(rc[1]) : null,
				output: body.replace(/(?:^|\n)rc=\d+\s*$/, "").trim(),
			};
		});
	}

	// Records which sub is in each slot; returns what changed since the last
	// read of this connection.
	private observeSlots(
		runtime: Runtime,
		subs: Pick<ActiveSub, "slot" | "subId">[],
	): SimChange[] {
		const after: SlotMap = new Map(subs.map((sub) => [sub.slot, sub.subId]));
		const before = runtime.slots;
		runtime.slots = after;
		return before ? diffSlots(before, after) : [];
	}

	private async simCheck(runtime: Runtime, reason: SimSignal): Promise<void> {
		const { devices } = this.options;
		const at = this.now().toISOString();
		let changes: SimChange[];
		try {
			const result = await devices.shell(runtime.serial, "dumpsys isub", {
				timeoutMs: 20_000,
			});
			const isub = new TextDecoder().decode(result.stdout);
			assertIsub(isub);
			changes = this.observeSlots(runtime, parseIsub(isub).active);
		} catch (error) {
			if (this.policy) this.fail(runtime, "sim", at, error, null);
			else
				this.options.log("sim-read-failed", {
					serial: runtime.serial,
					error: errorText(error),
				});
			return;
		}
		this.options.log("sim-signal", {
			serial: runtime.serial,
			reason,
			changes: changes.length,
		});
		const inserted = changes.flatMap((change) =>
			change.to === null ? [] : [change.to],
		);
		// Each new SIM's own data key, before anything else reads or records
		// the SIM: with ro.com.android.mobiledata unset (measured on the B30) a
		// new SIM's data defaults to on.
		if (this.policy?.radiosOff && inserted.length > 0)
			try {
				await this.apply(runtime.serial, dataOffCommands(inserted));
			} catch (error) {
				this.dataOffFailed(runtime, "sim", error);
			}
		if (this.policy && changes.length > 0)
			await this.runCheck(runtime, this.policy, "sim", changes);
		else devices.refresh(runtime.serial);
	}

	// Reads every rule, reverts radio and data drift, and, when the policy
	// keeps radios off, turns data off for SIMs over the threshold or new
	// ones. Never throws: a failure is recorded on the report and raises an
	// alert.
	private async runCheck(
		runtime: Runtime,
		policy: Policy,
		reason: CheckReason,
		known: SimChange[] = [],
	): Promise<CheckReport> {
		const { serial } = runtime;
		const { rules, radiosOff } = policy;
		const context = this.context(serial);
		const at = this.now().toISOString();
		// A data-off failure recorded after this point is newer than what this
		// check reads, so this check must not clear it.
		const started = ++this.sequence;
		let dataOffCause: DataOffCause | null =
			radiosOff && known.some((change) => change.to !== null) ? "sim" : null;
		try {
			const { snapshot, ms: readMs } = await this.read(serial, policy);
			const changes = [...known, ...this.observeSlots(runtime, snapshot.subs)];
			const inserted = radiosOff
				? changes.flatMap((change) =>
						change.to === null ? [] : [{ slot: change.slot, subId: change.to }],
					)
				: [];
			const before = evaluate(snapshot, context, rules);
			const usage = this.usage(snapshot);
			const over = usage.filter((sim) => sim.over);
			const steps = plan(snapshot, context, (rule) => rule.autoFix, rules);
			const dataOffSubs = [...over, ...inserted].map((sim) => sim.subId);
			const extra = dataOffSubs.length > 0 ? dataOffCommands(dataOffSubs) : [];
			const preset =
				radiosOff && reason === "connect"
					? presetDataOffCommands(snapshot)
					: [];
			if (inserted.length > 0) dataOffCause = "sim";
			else if (over.length > 0) dataOffCause ??= "data_threshold";
			let after = before;
			let afterSnapshot = snapshot;
			if (steps.length + extra.length + preset.length > 0) {
				// Safety first: radios and data off, then everything else (the
				// log buffer), in case the batch is cut short.
				await this.apply(serial, [
					...commandsOf(rules, steps, true),
					...extra,
					...preset,
					...commandsOf(rules, steps, false),
				]);
				({ snapshot: afterSnapshot } = await this.read(serial, policy));
				after = evaluate(afterSnapshot, context, rules);
			}
			const byId = (results: RuleResult[], id: string) =>
				results.find((result) => result.id === id);
			const dataOff = byId(after, "mobile_data")?.ok ?? false;
			if (dataOff) {
				if ((runtime.dataOffFailure?.seq ?? 0) < started)
					runtime.dataOffFailure = null;
			} else if (dataOffCause)
				this.dataOffFailed(
					runtime,
					dataOffCause,
					`mobile data still on: ${byId(after, "mobile_data")?.observed}`,
				);
			const reverted = steps.map((step) => ({
				rule: step.rule,
				before: byId(before, step.rule)?.observed ?? "",
				after: byId(after, step.rule)?.observed ?? "",
				fixed: byId(after, step.rule)?.ok ?? false,
			}));
			const events: NewEvent[] = [];
			const phoneId = this.phoneId(serial);
			// Drift is logged whether or not the revert took. A routine fix (the
			// log buffer) is logged only when it took; a failed one shows on the
			// phone's card instead.
			const routine = reverted.filter((revert) =>
				fixEventOf(rules, revert.rule),
			);
			const drifted = reverted.filter(
				(revert) => !fixEventOf(rules, revert.rule),
			);
			for (const revert of drifted)
				events.push({ phoneId, kind: "drift", detail: { ...revert, reason } });
			for (const revert of routine)
				if (revert.fixed)
					events.push({
						phoneId,
						kind: fixEventOf(rules, revert.rule) as string,
						detail: { ...revert, reason },
					});
			for (const sim of over)
				if (!runtime.alarmed.has(sim.subId))
					events.push({
						phoneId,
						kind: "data_threshold",
						detail: {
							subId: sim.subId,
							slot: sim.slot,
							rx: sim.rx,
							tx: sim.tx,
							thresholdBytes: radiosOff?.dataThresholdBytes,
							uids: sim.uids.slice(0, 5),
							dataOff,
						},
					});
			runtime.alarmed = new Set(over.map((sim) => sim.subId));
			for (const sim of inserted) {
				const sub = afterSnapshot.subs.find((s) => s.subId === sim.subId);
				events.push({
					phoneId,
					kind: "sim.secured",
					detail: {
						slot: sim.slot,
						subId: sim.subId,
						dataOff,
						mmsWithDataOff:
							sub?.dataPolicies?.includes(MMS_ALWAYS_ALLOWED) ?? false,
					},
				});
			}
			this.publish(events);
			if (drifted.length > 0)
				this.options.log("drift", { serial, reason, reverted: drifted });
			for (const revert of routine)
				this.options.log(
					(fixEventOf(rules, revert.rule) as string).replaceAll("_", "-"),
					{ serial, reason, ...revert },
				);
			runtime.check = {
				at,
				reason,
				readMs,
				results: after,
				usage,
				reverted,
				error: null,
			};
			this.refreshAlerts();
			// Inventory: placements, numbers, and the sim.added/placed/removed
			// events come from the device manager's identity read.
			if (changes.length > 0) this.options.devices.refresh(serial);
			return runtime.check;
		} catch (error) {
			return this.fail(runtime, reason, at, error, dataOffCause);
		}
	}

	// Records a failed check, keeping the last known rule results, and raises
	// its alert; a failure while data had to go off raises a critical one.
	private fail(
		runtime: Runtime,
		reason: CheckReason,
		at: string,
		error: unknown,
		dataOffCause: DataOffCause | null,
	): CheckReport {
		if (dataOffCause) this.dataOffFailed(runtime, dataOffCause, error);
		runtime.check = {
			at,
			reason,
			readMs: 0,
			results: runtime.check?.results ?? [],
			usage: runtime.check?.usage ?? [],
			reverted: [],
			error: errorText(error),
		};
		this.options.log("policy-check-failed", {
			serial: runtime.serial,
			reason,
			error: errorText(error),
		});
		this.refreshAlerts();
		return runtime.check;
	}

	private async runProvision(
		runtime: Runtime,
		policy: Policy,
	): Promise<ProvisionReport> {
		const { serial } = runtime;
		const context = this.context(serial);
		const started = performance.now();
		const at = this.now().toISOString();
		let report: ProvisionReport;
		try {
			const { rules } = policy;
			const { snapshot } = await this.read(serial, policy);
			const changes = this.observeSlots(runtime, snapshot.subs);
			const steps = plan(snapshot, context, () => true, rules);
			const ran = await this.apply(serial, [
				...commandsOf(rules, steps, true),
				...(policy.radiosOff ? presetDataOffCommands(snapshot) : []),
				...commandsOf(rules, steps, false),
			]);
			if (changes.length > 0) this.options.devices.refresh(serial);
			const { snapshot: after } = await this.read(serial, policy);
			const results = evaluate(after, context, rules);
			report = {
				at,
				durationMs: Math.round(performance.now() - started),
				steps: steps.map((step) => ({
					rule: step.rule,
					commands: step.commands,
					failed: ran
						.filter(
							(run) =>
								step.commands.includes(run.command) && run.exitCode !== 0,
						)
						.map((run) => ({ command: run.command, output: run.output })),
				})),
				results,
				ok: results.every((result) => result.ok),
				error: null,
			};
			runtime.check = {
				at,
				reason: "manual",
				readMs: 0,
				results,
				usage: this.usage(after),
				reverted: [],
				error: null,
			};
		} catch (error) {
			report = {
				at,
				durationMs: Math.round(performance.now() - started),
				steps: [],
				results: [],
				ok: false,
				error: errorText(error),
			};
		}
		runtime.provision = report;
		runtime.provisionLoaded = true;
		const phoneId = this.phoneId(serial);
		this.publish([
			{
				phoneId,
				kind: "provision",
				detail: {
					ok: report.ok,
					changed: report.steps.map((step) => step.rule),
					failedCommands: report.steps.flatMap((step) => step.failed),
					failedRules: report.results
						.filter((result) => !result.ok)
						.map((result) => result.id),
					durationMs: report.durationMs,
					error: report.error,
				},
			},
		]);
		this.options.log("provision", {
			serial,
			ok: report.ok,
			changed: report.steps.map((step) => step.rule),
			error: report.error,
		});
		this.refreshAlerts();
		return report;
	}

	// The data watch, which only a radios-off policy reads for.
	private usage(snapshot: Snapshot): SimDataUsage[] {
		const threshold = this.dataThresholdBytes;
		if (threshold === null) return [];
		const now = snapshot.phoneClock ?? Math.floor(this.now().getTime() / 1000);
		return internetUsage(parseNetstats(snapshot.netstats), now).map((sim) => ({
			...sim,
			slot: snapshot.subs.find((sub) => sub.subId === sim.subId)?.slot ?? null,
			over: sim.rx + sim.tx > threshold,
		}));
	}

	private mobileBytes(serial: string): { rx: number; tx: number } | null {
		const usage = this.runtimes.get(serial)?.check?.usage;
		if (!usage) return null;
		return usage.reduce(
			(total, sim) => ({ rx: total.rx + sim.rx, tx: total.tx + sim.tx }),
			{ rx: 0, tx: 0 },
		);
	}

	private refreshAlerts(): void {
		const now = this.now();
		const { devices } = this.options;
		const next: Alert[] = this.health.alerts(devices.list(), now);
		for (const runtime of this.runtimes.values()) {
			// Kept while the phone is offline too: only a check that finds data
			// off clears it.
			const failure = runtime.dataOffFailure;
			if (failure)
				next.push({
					key: `sim_unsecured:${runtime.serial}`,
					kind: "sim_unsecured",
					serial: runtime.serial,
					severity: "critical",
					message: `${failure.cause === "sim" ? "A SIM changed" : "Mobile data went over the threshold"} and turning mobile data off failed: ${failure.error}`,
					since: failure.at,
				});
			if (devices.get(runtime.serial)?.status !== "online") continue;
			const check = runtime.check;
			if (!check) continue;
			const since = check.at;
			if (check.error)
				next.push({
					key: `check_failed:${runtime.serial}`,
					kind: "check_failed",
					serial: runtime.serial,
					severity: "warning",
					message: `Policy check failed: ${check.error}`,
					since,
				});
			for (const result of check.results) {
				if (result.ok) continue;
				// A radio or data setting the fix could not turn off is critical;
				// any other rule, the log buffer included, is a card item.
				const unfixedRadio =
					result.autoFix &&
					!!ruleOf(this.policy?.rules ?? [], result.id)?.safety;
				next.push({
					key: `${unfixedRadio ? "drift_unfixed" : "policy"}:${runtime.serial}:${result.id}`,
					kind: unfixedRadio ? "drift_unfixed" : "policy",
					serial: runtime.serial,
					severity: result.severity,
					message: unfixedRadio
						? `${result.describe}: still ${result.observed} after the automatic fix.`
						: `${result.describe}: ${result.observed} (want ${result.want}).${result.instruction ? ` ${result.instruction}` : ""}`,
					since,
				});
			}
			for (const sim of check.usage)
				if (sim.over)
					next.push({
						key: `data_threshold:${runtime.serial}:${sim.subId}`,
						kind: "data_threshold",
						serial: runtime.serial,
						severity: "critical",
						message: `SIM ${sim.slot === null ? `sub ${sim.subId}` : sim.slot + 1} used ${sim.rx + sim.tx} bytes of mobile data in 24 h; data turned off again.`,
						since,
					});
		}
		const previous = this.current;
		const merged = new Map<string, Alert>();
		for (const alert of next) {
			const kept = previous.get(alert.key);
			merged.set(alert.key, kept ? { ...alert, since: kept.since } : alert);
		}
		const events: NewEvent[] = [];
		for (const [key, alert] of merged)
			if (!previous.has(key))
				events.push({
					phoneId: this.phoneId(alert.serial),
					kind: "alert.raised",
					detail: {
						key,
						kind: alert.kind,
						severity: alert.severity,
						message: alert.message,
					},
				});
		for (const [key, alert] of previous)
			if (!merged.has(key))
				events.push({
					phoneId: this.phoneId(alert.serial),
					kind: "alert.cleared",
					detail: { key, kind: alert.kind },
				});
		this.current = merged;
		this.publish(events);
	}

	private lastProvision(serial: string): ProvisionReport | null {
		const row = this.options.db
			.query<{ at: string; detail_json: string }, [string]>(
				`SELECT e.at, e.detail_json FROM device_events e JOIN phones p ON p.id = e.phone_id
				WHERE p.serial = ? AND e.kind = 'provision' ORDER BY e.id DESC LIMIT 1`,
			)
			.get(serial);
		if (!row) return null;
		const detail = JSON.parse(row.detail_json) as {
			ok?: boolean;
			changed?: string[];
			durationMs?: number;
			error?: string | null;
		};
		return {
			at: row.at,
			durationMs: detail.durationMs ?? 0,
			steps: (detail.changed ?? []).map((rule) => ({
				rule,
				commands: [],
				failed: [],
			})),
			results: [],
			ok: detail.ok ?? false,
			error: detail.error ?? null,
		};
	}

	private phoneId(serial: string): number | null {
		return (
			this.options.db
				.query<{ id: number }, [string]>(
					"SELECT id FROM phones WHERE serial = ?",
				)
				.get(serial)?.id ?? null
		);
	}

	private publish(events: NewEvent[]): void {
		if (events.length === 0) return;
		for (const row of insertEvents(
			this.options.db,
			events,
			this.now().toISOString(),
		))
			this.options.events.publish(row);
	}
}

// An isub dump that lists subscriptions, even none. Anything else (an error,
// a truncated dump) would read as every SIM removed.
function assertIsub(text: string): void {
	if (!text.includes("Active subscriptions:"))
		throw new Error("dumpsys isub printed no subscription list");
}

const ruleOf = (rules: readonly Rule[], id: string) =>
	rules.find((rule) => rule.id === id);
const fixEventOf = (rules: readonly Rule[], id: string) =>
	ruleOf(rules, id)?.fixEvent;

// The commands of the safety (radio and data) steps, or of the others.
function commandsOf(
	rules: readonly Rule[],
	steps: { rule: string; commands: string[] }[],
	safety: boolean,
): string[] {
	return steps
		.filter((step) => (ruleOf(rules, step.rule)?.safety ?? false) === safety)
		.flatMap((step) => step.commands);
}

// Turns mobile data off for the phone and for each listed sub.
export function dataOffCommands(subIds: number[]): string[] {
	return [
		"svc data disable",
		"settings put global mobile_data 0",
		...[...new Set(subIds)].map(
			(subId) => `settings put global mobile_data${subId} 0`,
		),
	];
}

function errorText(error: unknown): string {
	return error instanceof Error ? error.message : String(error);
}
