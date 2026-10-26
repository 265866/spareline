import {
	AlertTriangle,
	CircleAlert,
	RefreshCw,
	ShieldCheck,
	Wand2,
} from "lucide-react";
import { useState, useSyncExternalStore } from "react";
import { api, type StatusView } from "../api";
import { ago } from "../format";
import {
	alertText,
	bannerAlerts,
	needsSetup,
	provisionNote,
	safetyStatus,
} from "../phone-status";

// One /status poll shared by the alert banner and every phone card.
const store = {
	data: undefined as StatusView | undefined,
	error: null as string | null,
	listeners: new Set<() => void>(),
	timer: undefined as ReturnType<typeof setInterval> | undefined,
	version: 0,
};

function loadStatus(): void {
	api<StatusView>("/status")
		.then((data) => {
			store.data = data;
			store.error = null;
		})
		.catch((reason: unknown) => {
			store.error = reason instanceof Error ? reason.message : String(reason);
		})
		.finally(() => {
			store.version++;
			for (const listener of store.listeners) listener();
		});
}

function subscribe(listener: () => void): () => void {
	store.listeners.add(listener);
	if (store.listeners.size === 1) {
		loadStatus();
		store.timer = setInterval(loadStatus, 15_000);
		window.addEventListener("device-event", loadStatus);
	}
	return () => {
		store.listeners.delete(listener);
		if (store.listeners.size === 0) {
			clearInterval(store.timer);
			window.removeEventListener("device-event", loadStatus);
		}
	};
}

function useStatus() {
	useSyncExternalStore(subscribe, () => store.version);
	return { data: store.data, error: store.error, reload: loadStatus };
}

const SEVERITY_COLOR = {
	critical: "text-red-700 dark:text-red-400",
	warning: "text-amber-700 dark:text-amber-400",
	info: "text-zinc-600 dark:text-zinc-300",
} as const;

// Shown at the top of the Phones page only while there are alerts the phone
// cards do not already show: red when any is critical, amber otherwise.
export function MonitorAlerts() {
	const monitor = useStatus();
	const resultsFor = (serial: string) =>
		monitor.data?.phones.find((phone) => phone.serial === serial)?.policy?.check
			?.results;
	const alerts = bannerAlerts(monitor.data?.alerts ?? [], resultsFor);
	if (monitor.error)
		return (
			<div className="rounded-lg border border-amber-300 bg-amber-50 p-3 text-sm text-amber-900 dark:border-amber-800 dark:bg-amber-950/60 dark:text-amber-200">
				Could not load the phone status: {monitor.error}
			</div>
		);
	if (alerts.length === 0) return null;
	const critical = alerts.some((alert) => alert.severity === "critical");
	return (
		<section
			className={`flex gap-3 rounded-lg border p-3 text-sm ${
				critical
					? "border-red-300 bg-red-50 text-red-900 dark:border-red-800 dark:bg-red-950/60 dark:text-red-200"
					: "border-amber-300 bg-amber-50 text-amber-900 dark:border-amber-800 dark:bg-amber-950/60 dark:text-amber-200"
			}`}
		>
			<AlertTriangle size={18} className="mt-0.5 shrink-0" />
			<div className="flex flex-col gap-1">
				<h2 className="font-semibold">
					{alerts.length === 1
						? "1 thing needs attention"
						: `${alerts.length} things need attention`}
				</h2>
				<ul className="flex flex-col gap-1">
					{alerts.map((alert) => (
						<li key={alert.key}>
							<span className="font-mono text-xs">{alert.serial}</span>{" "}
							{alertText(alert, resultsFor(alert.serial))}
						</li>
					))}
				</ul>
			</div>
		</section>
	);
}

function formatBytes(bytes: number): string {
	if (bytes < 1000) return `${bytes} B`;
	if (bytes < 1_000_000) return `${(bytes / 1000).toFixed(1)} kB`;
	return `${(bytes / 1_000_000).toFixed(1)} MB`;
}

function useAction(serial: string, reload: () => void) {
	const [running, setRunning] = useState<"check" | "provision" | null>(null);
	const [error, setError] = useState<string | null>(null);
	const run = async (action: "check" | "provision") => {
		setRunning(action);
		setError(null);
		try {
			// A JSON body makes the request one a cross-site form cannot send.
			await api(`/phones/${encodeURIComponent(serial)}/${action}`, {
				method: "POST",
				body: {},
			});
		} catch (reason) {
			setError(reason instanceof Error ? reason.message : String(reason));
		} finally {
			setRunning(null);
			reload();
		}
	};
	return { running, error, run };
}

function ActionButton({
	onClick,
	busy,
	disabled,
	icon,
	children,
	title,
}: {
	onClick: () => void;
	busy: boolean;
	disabled?: boolean;
	icon: React.ReactNode;
	children: React.ReactNode;
	title: string;
}) {
	return (
		<button
			type="button"
			onClick={onClick}
			disabled={busy || disabled}
			title={title}
			className="inline-flex items-center gap-1 rounded-md border px-2 py-1 text-xs font-medium hover:bg-zinc-50 disabled:opacity-50 dark:hover:bg-zinc-800"
		>
			{busy ? <RefreshCw size={12} className="animate-spin" /> : icon}
			{children}
		</button>
	);
}

function setupConfirm(profile: { name: string; setUp: string[] }): string {
	const removes = profile.setUp.includes("Unneeded apps removed");
	return `Set up this phone with the ${profile.name} profile?

${profile.setUp.map((line) => `- ${line}`).join("\n")}${removes ? "\n\nRemoving an app deletes its data. The app can be reinstalled later, but its data cannot be restored." : ""}`;
}

// The monitor's part of a phone card: the data alarm, "Policy OK" or only
// the rules the phone fails, and Set up phone while it is not set up yet.
export function PhoneMonitor({ serial }: { serial: string }) {
	const monitor = useStatus();
	const entry = monitor.data?.phones.find((phone) => phone.serial === serial);
	const policy = entry?.policy ?? null;
	const action = useAction(serial, monitor.reload);
	const profile = monitor.data?.profile ?? null;
	if (!entry || !profile) return null;
	const check = policy?.check;
	const status = safetyStatus(check);
	const over = check?.usage.filter((sim) => sim.over) ?? [];
	const provision = policy?.provision;
	const note = provisionNote(provision);
	const online = entry.status === "online";
	const settingUp =
		action.running === "provision" || policy?.busy === "provision";
	const showSetup =
		settingUp || (online && check != null && needsSetup(check.results));
	return (
		<div className="flex flex-col gap-2">
			{policy?.dataAlarm && (
				<div className="flex gap-2 rounded-lg bg-red-600 p-3 text-sm text-white">
					<AlertTriangle size={18} className="shrink-0" />
					<span>
						Mobile data used:{" "}
						{over
							.map(
								(sim) =>
									`SIM ${sim.slot === null ? `sub ${sim.subId}` : sim.slot + 1} ${formatBytes(sim.rx + sim.tx)}`,
							)
							.join(", ")}{" "}
						in 24 h. Data was turned off again.
					</span>
				</div>
			)}
			<div className="flex flex-wrap items-center justify-between gap-2 text-sm">
				{status.kind === "unchecked" ? (
					<span className="text-zinc-400">Policy not checked yet</span>
				) : status.kind === "ok" ? (
					<span
						className="inline-flex items-center gap-1.5 font-medium text-emerald-700 dark:text-emerald-400"
						title={`All ${status.rules} rules pass · checked ${ago(status.at)}`}
					>
						<ShieldCheck size={15} /> Policy OK
					</span>
				) : (
					<span className="font-medium">
						Policy: {status.problems.length} problem
						{status.problems.length === 1 ? "" : "s"}
					</span>
				)}
				<span className="flex gap-1">
					<ActionButton
						onClick={() => action.run("check")}
						busy={action.running === "check" || policy?.busy === "check"}
						disabled={!online || action.running !== null}
						icon={<RefreshCw size={12} />}
						title={`Check every rule now${check ? ` (last checked ${ago(check.at)})` : ""}`}
					>
						Check now
					</ActionButton>
					{showSetup && (
						<ActionButton
							onClick={() => {
								if (window.confirm(setupConfirm(profile)))
									void action.run("provision");
							}}
							busy={settingUp}
							disabled={!online || action.running !== null}
							icon={<Wand2 size={12} />}
							title={`Apply the ${profile.name} profile, then verify`}
						>
							Set up phone
						</ActionButton>
					)}
				</span>
			</div>
			{status.kind === "problems" && (
				<ul className="flex flex-col gap-1 text-sm">
					{status.problems.map((problem) => (
						<li
							key={problem.id}
							className={`flex gap-1.5 ${SEVERITY_COLOR[problem.severity]}`}
						>
							<CircleAlert size={15} className="mt-0.5 shrink-0" />
							<span>{problem.text}</span>
						</li>
					))}
				</ul>
			)}
			{note && (
				<span className="text-xs text-red-600" title={provision?.at}>
					{note}
				</span>
			)}
			{action.error && (
				<span className="text-xs text-red-600">{action.error}</span>
			)}
		</div>
	);
}
