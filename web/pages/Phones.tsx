import {
	AlertTriangle,
	BatteryCharging,
	BatteryFull,
	BatteryLow,
	BatteryMedium,
	Plug,
	ShieldAlert,
	Smartphone,
	Thermometer,
	Usb,
} from "lucide-react";
import { type PhoneView, useResource } from "../api";
import { Badge, Empty, SignalBars, StatusBadge } from "../components/ui";
import { ago, formatNumber, SERVICE_LABEL } from "../format";
import { linkProps } from "../router";

export function PhonesPage() {
	const phones = useResource<{ phones: PhoneView[] }>("/phones");
	const list = phones.data?.phones;
	const online = list?.filter((phone) => phone.status === "online").length ?? 0;
	return (
		<div className="flex flex-col gap-4">
			<div>
				<h1 className="text-xl font-semibold">Phones</h1>
				<p className="text-sm text-zinc-500 dark:text-zinc-400">
					{list ? `${online} of ${list.length} online` : "Loading…"}
				</p>
			</div>
			{phones.error && (
				<Badge tone="bad">Could not load phones: {phones.error}</Badge>
			)}
			{list?.length === 0 && (
				<Empty icon={<Smartphone size={32} />} title="No phones yet">
					Plug a phone into the host with USB debugging on. It appears here
					within a second.
				</Empty>
			)}
			<div className="grid grid-cols-1 gap-4 lg:grid-cols-2 2xl:grid-cols-3">
				{list?.map((phone) => (
					<PhoneCard key={phone.serial} phone={phone} />
				))}
			</div>
		</div>
	);
}

function BatteryLine({ phone }: { phone: PhoneView }) {
	const battery = phone.battery;
	if (!battery)
		return <span className="text-zinc-400">No battery reading</span>;
	const Icon =
		battery.status === "charging"
			? BatteryCharging
			: battery.level > 70
				? BatteryFull
				: battery.level > 30
					? BatteryMedium
					: BatteryLow;
	const hot = battery.temperatureC >= 45;
	return (
		<span className="inline-flex flex-wrap items-center gap-3">
			<span
				className="inline-flex items-center gap-1"
				title={battery.status.replace("_", " ")}
			>
				<Icon size={16} className={battery.level <= 15 ? "text-red-500" : ""} />
				{battery.level}%
			</span>
			<span
				className={`inline-flex items-center gap-1 ${hot ? "font-medium text-red-600" : ""}`}
			>
				<Thermometer size={16} />
				{battery.temperatureC.toFixed(1)} °C
			</span>
			{battery.plugged.length > 0 && (
				<span className="inline-flex items-center gap-1 text-zinc-500 dark:text-zinc-400">
					<Plug size={14} />
					{battery.plugged.join(", ").toUpperCase()}
				</span>
			)}
		</span>
	);
}

function PhoneCard({ phone }: { phone: PhoneView }) {
	const title =
		[phone.manufacturer, phone.model].filter(Boolean).join(" ") ||
		"Unknown phone";
	return (
		<article className="flex flex-col gap-3 rounded-xl border bg-white p-4 shadow-sm dark:bg-zinc-900">
			<header className="flex items-start justify-between gap-3">
				<div className="min-w-0">
					<h2 className="truncate font-semibold">{title}</h2>
					<p className="font-mono text-xs text-zinc-500 dark:text-zinc-400">
						{phone.serial}
						{phone.androidVersion && (
							<span className="font-sans">
								{" "}
								· Android {phone.androidVersion}
							</span>
						)}
					</p>
				</div>
				<StatusBadge phone={phone} />
			</header>

			{phone.status === "needs_auth" && (
				<div className="flex gap-2 rounded-lg bg-amber-50 p-3 text-sm text-amber-900 dark:bg-amber-950/60 dark:text-amber-200">
					<ShieldAlert size={18} className="shrink-0" />
					<span>
						On the phone, tap <b>Allow</b> on “Allow USB debugging?” and tick{" "}
						<b>Always allow from this computer</b>.
					</span>
				</div>
			)}
			{phone.status === "no_permissions" && (
				<div className="flex gap-2 rounded-lg bg-red-50 p-3 text-sm text-red-900 dark:bg-red-950/60 dark:text-red-200">
					<Usb size={18} className="shrink-0" />
					<span>
						The host's adb server cannot open this USB device. Check the udev
						rules and the plugdev group.
					</span>
				</div>
			)}
			{(phone.battery || phone.status === "online") && (
				<div className="text-sm">
					<BatteryLine phone={phone} />
				</div>
			)}

			<div className="flex flex-col divide-y rounded-lg border">
				{phone.slots.length === 0 && (
					<div className="p-3 text-sm text-zinc-500">
						No SIM information yet
					</div>
				)}
				{phone.slots.map((slot) => (
					<div key={slot.slot} className="flex flex-col gap-1.5 p-3 text-sm">
						<div className="flex flex-wrap items-center justify-between gap-2">
							<span className="font-medium">
								SIM {slot.slot + 1}
								{slot.sim?.carrier && (
									<span className="font-normal text-zinc-500 dark:text-zinc-400">
										{" "}
										· {slot.sim.carrier}
									</span>
								)}
							</span>
							<span className="flex items-center gap-2">
								{slot.radio && slot.simState !== "ABSENT" && (
									<>
										<Badge
											tone={
												slot.radio.voice === "in_service"
													? "good"
													: slot.radio.voice === "out_of_service"
														? "warn"
														: "neutral"
											}
											title={slot.radio.operator ?? undefined}
										>
											{SERVICE_LABEL[slot.radio.voice] ?? slot.radio.voice}
										</Badge>
										<SignalBars
											level={slot.radio.signalLevel}
											title={[
												slot.radio.signalType,
												slot.radio.signalDbm !== null
													? `${slot.radio.signalDbm} dBm`
													: null,
											]
												.filter(Boolean)
												.join(" ")}
										/>
										{slot.radio.signalDbm !== null && (
											<span className="text-xs text-zinc-500 tabular-nums dark:text-zinc-400">
												{slot.radio.signalDbm} dBm
											</span>
										)}
										{slot.radio.dataEnabled === true && (
											<Badge
												tone="bad"
												title="Mobile data is enabled on this SIM"
											>
												<AlertTriangle size={12} /> Data on
											</Badge>
										)}
										{slot.radio.dataEnabled === false && (
											<Badge title="Mobile data is disabled on this SIM">
												Data off
											</Badge>
										)}
									</>
								)}
								{slot.simState === "ABSENT" && (
									<span className="text-xs text-zinc-400">Empty</span>
								)}
							</span>
						</div>
						{slot.number && (
							<a
								{...linkProps(`/numbers/${slot.number.id}`)}
								className="w-fit font-medium tabular-nums hover:underline"
							>
								{slot.number.e164
									? formatNumber(slot.number.e164)
									: "Number not detected — set it"}
								{slot.number.source === "manual" && (
									<span className="ml-1 text-xs font-normal text-zinc-500">
										(set by hand)
									</span>
								)}
							</a>
						)}
						{slot.sim && (
							<span className="font-mono text-xs text-zinc-500 dark:text-zinc-400">
								ICCID {slot.sim.iccid}
							</span>
						)}
						{!slot.sim && slot.simState && slot.simState !== "ABSENT" && (
							<span className="text-xs text-zinc-500">
								SIM state: {slot.simState}
							</span>
						)}
					</div>
				))}
			</div>

			{phone.imeis.length > 0 && (
				<div className="flex flex-wrap gap-x-4 gap-y-1 font-mono text-xs text-zinc-500 dark:text-zinc-400">
					{phone.imeis.map((imei, index) => (
						<span key={imei}>
							IMEI {index + 1} {imei}
						</span>
					))}
				</div>
			)}

			{(phone.probeError || phone.warnings.length > 0) && (
				<details className="text-xs text-amber-700 dark:text-amber-400">
					<summary className="cursor-pointer">
						{phone.probeError
							? "Probe failed"
							: `${phone.warnings.length} probe warning${phone.warnings.length > 1 ? "s" : ""}`}
					</summary>
					<ul className="mt-1 list-disc pl-4">
						{phone.probeError && <li>{phone.probeError}</li>}
						{phone.warnings.map((warning) => (
							<li key={warning}>{warning}</li>
						))}
					</ul>
				</details>
			)}
			<footer className="text-xs text-zinc-400 dark:text-zinc-500">
				{phone.healthAt
					? `Updated ${ago(phone.healthAt)}`
					: `First seen ${ago(phone.firstSeenAt)}`}
			</footer>
		</article>
	);
}
