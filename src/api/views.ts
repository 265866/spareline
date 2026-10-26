import type { DeviceState } from "../adb/device-list";
import type { Devices, LivePhone } from "../devices/manager";
import type { Battery as ProbeBattery } from "../devices/parse";
import type { NumberRow, PhoneRow } from "../inventory/store";
import { phoneStatus } from "../inventory/sync";
import type { NumberView, PhoneView, SlotView } from "./schema";

// Joins what the database knows about a phone with what DeviceManager sees
// now. A phone this process has not seen yet shows its last stored state.
export function phoneView(
	row: PhoneRow,
	live: LivePhone | undefined,
): PhoneView {
	const adbState = live?.adbState ?? row.lastState;
	const slotNumbers = new Set<number>([
		...(live?.identity?.slots.map((slot) => slot.slot) ?? []),
		...row.placements.map((placement) => placement.slot),
	]);
	const slots: SlotView[] = [...slotNumbers]
		.sort((a, b) => a - b)
		.map((slot) => {
			const placement = row.placements.find(
				(candidate) => candidate.slot === slot,
			);
			return {
				slot,
				simState:
					live?.health?.simStates[slot] ??
					live?.identity?.slots.find((candidate) => candidate.slot === slot)
						?.simState ??
					null,
				radio:
					live?.health?.radios.find((radio) => radio.slot === slot) ?? null,
				sim: placement ? { ...placement.sim, subId: placement.subId } : null,
				number: placement?.number ?? null,
			};
		});
	return {
		id: row.id,
		serial: row.serial,
		site: row.site,
		manufacturer: row.manufacturer,
		model: row.model,
		androidVersion: row.androidVersion,
		imeis: row.imeis,
		location: row.location,
		hubPort: row.hubPort,
		costCents: row.costCents,
		acquiredOn: row.acquiredOn,
		status: live?.status ?? phoneStatus(row.lastState as DeviceState),
		adbState,
		statusSince: live?.since ?? null,
		firstSeenAt: row.firstSeenAt,
		lastOnlineAt: row.lastOnlineAt,
		battery: batteryView(live?.health?.battery ?? null),
		slots,
		healthAt: live?.healthAt ?? null,
		identityAt: live?.identityAt ?? null,
		warnings: live?.identity?.warnings ?? [],
		probeError: live?.probeError ?? null,
	};
}

export function numberView(row: NumberRow, devices: Devices): NumberView {
	const live =
		row.phone && row.phone.removedAt === null
			? devices.get(row.phone.serial)
			: undefined;
	return {
		...row,
		phoneStatus:
			row.phone && row.phone.removedAt === null
				? (live?.status ?? "offline")
				: null,
		radio:
			live?.health?.radios.find((radio) => radio.slot === row.phone?.slot) ??
			null,
	};
}

export function batteryView(
	battery: ProbeBattery | null,
): PhoneView["battery"] {
	if (!battery) return null;
	return {
		level: battery.level,
		temperatureC: battery.temperatureDeciC / 10,
		status: battery.status,
		plugged: battery.plugged,
	};
}
