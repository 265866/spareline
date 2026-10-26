import type { Database } from "bun:sqlite";
import type { LivePhone } from "../devices/manager";
import type { Severity } from "./policy";

export const OFFLINE_ALERT_MS = 5 * 60_000;
export const OUT_OF_SERVICE_ALERT_MS = 30 * 60_000;
// The hot-battery alert raises above 45.0 °C and clears below 43.0 °C, so a
// battery hovering at the limit does not flap.
export const HOT_BATTERY_DECI_C = 450;
export const COOL_BATTERY_DECI_C = 430;
// In-service samples in a row that end an out-of-service spell.
export const IN_SERVICE_SAMPLES_TO_CLEAR = 3;
export const BUCKET_MS = 5 * 60_000;
const RETENTION_MS = 90 * 24 * 60 * 60_000;

export type AlertKind =
	| "offline"
	| "no_permissions"
	| "out_of_service"
	| "battery_hot"
	| "policy"
	| "drift_unfixed"
	| "data_threshold"
	| "check_failed"
	| "sim_unsecured";

export type Alert = {
	// Stable while the condition lasts, for raise and clear events.
	key: string;
	kind: AlertKind;
	serial: string;
	severity: Severity;
	message: string;
	since: string;
};

type Bucket = {
	start: number;
	samples: number;
	batterySum: number;
	batteryCount: number;
	tempMax: number | null;
	// By slot: lowest signal level, dBm sum and count, samples in service.
	slots: Map<
		number,
		{
			minLevel: number;
			dbmSum: number;
			dbmCount: number;
			inService: number;
			samples: number;
		}
	>;
};

// Samples every phone's latest health once a minute, keeps the 5-minute
// aggregate in memory, and writes one row per phone per 5 minutes, which
// keeps SD card writes small. It also tracks how long each SIM has been out
// of service, for the alert.
export class HealthRecorder {
	private readonly buckets = new Map<string, Bucket>();
	// serial:slot -> first sample not in service, and the in-service samples
	// in a row since.
	private readonly outOfService = new Map<
		string,
		{ since: number; inServiceStreak: number }
	>();
	// serial -> when the battery went over the limit, and its last reading.
	private readonly hot = new Map<
		string,
		{ since: string; temperatureDeciC: number }
	>();
	private lastPrune = 0;

	constructor(private readonly db: Database) {}

	sample(
		phones: LivePhone[],
		at: Date,
		mobileBytes: (serial: string) => { rx: number; tx: number } | null,
	): void {
		const start = Math.floor(at.getTime() / BUCKET_MS) * BUCKET_MS;
		for (const phone of phones) {
			if (phone.status !== "online" || !phone.health) {
				for (const key of this.outOfService.keys())
					if (key.startsWith(`${phone.serial}:`)) this.outOfService.delete(key);
				this.hot.delete(phone.serial);
				continue;
			}
			let bucket = this.buckets.get(phone.serial);
			if (bucket && bucket.start !== start) {
				this.write(phone.serial, bucket, mobileBytes(phone.serial));
				bucket = undefined;
			}
			bucket ??= {
				start,
				samples: 0,
				batterySum: 0,
				batteryCount: 0,
				tempMax: null,
				slots: new Map(),
			};
			this.buckets.set(phone.serial, bucket);
			bucket.samples++;
			const battery = phone.health.battery;
			if (battery) {
				bucket.batterySum += battery.level;
				bucket.batteryCount++;
				bucket.tempMax = Math.max(
					bucket.tempMax ?? battery.temperatureDeciC,
					battery.temperatureDeciC,
				);
				const temperatureDeciC = battery.temperatureDeciC;
				const hot = this.hot.get(phone.serial);
				if (
					temperatureDeciC > HOT_BATTERY_DECI_C ||
					(hot && temperatureDeciC >= COOL_BATTERY_DECI_C)
				)
					this.hot.set(phone.serial, {
						since: hot?.since ?? at.toISOString(),
						temperatureDeciC,
					});
				else this.hot.delete(phone.serial);
			}
			for (const radio of phone.health.radios) {
				const simState = phone.health.simStates[radio.slot];
				const key = `${phone.serial}:${radio.slot}`;
				if (simState !== "LOADED") {
					this.outOfService.delete(key);
					continue;
				}
				const slot = bucket.slots.get(radio.slot) ?? {
					minLevel: radio.signalLevel,
					dbmSum: 0,
					dbmCount: 0,
					inService: 0,
					samples: 0,
				};
				slot.samples++;
				slot.minLevel = Math.min(slot.minLevel, radio.signalLevel);
				if (radio.signalDbm !== null) {
					slot.dbmSum += radio.signalDbm;
					slot.dbmCount++;
				}
				const spell = this.outOfService.get(key);
				if (radio.voice === "in_service") {
					slot.inService++;
					if (spell && ++spell.inServiceStreak >= IN_SERVICE_SAMPLES_TO_CLEAR)
						this.outOfService.delete(key);
				} else if (spell) {
					spell.inServiceStreak = 0;
				} else {
					this.outOfService.set(key, {
						since: at.getTime(),
						inServiceStreak: 0,
					});
				}
				bucket.slots.set(radio.slot, slot);
			}
		}
		if (at.getTime() - this.lastPrune > 24 * 60 * 60_000) {
			this.lastPrune = at.getTime();
			this.db.run("DELETE FROM health_5m WHERE bucket < ?", [
				new Date(at.getTime() - RETENTION_MS).toISOString(),
			]);
		}
	}

	// Writes the partial buckets, at shutdown.
	flush(mobileBytes: (serial: string) => { rx: number; tx: number } | null) {
		for (const [serial, bucket] of this.buckets)
			this.write(serial, bucket, mobileBytes(serial));
		this.buckets.clear();
	}

	alerts(phones: LivePhone[], now: Date): Alert[] {
		const alerts: Alert[] = [];
		const nowMs = now.getTime();
		for (const phone of phones) {
			if (phone.status === "no_permissions") {
				alerts.push({
					key: `no_permissions:${phone.serial}`,
					kind: "no_permissions",
					serial: phone.serial,
					severity: "critical",
					message:
						"The host's adb server cannot open this USB device (udev rules or plugdev group).",
					since: phone.since,
				});
				continue;
			}
			if (phone.status !== "online") {
				if (nowMs - Date.parse(phone.since) > OFFLINE_ALERT_MS)
					alerts.push({
						key: `offline:${phone.serial}`,
						kind: "offline",
						serial: phone.serial,
						severity: "critical",
						message:
							phone.status === "needs_auth"
								? "Waiting for Allow USB debugging for more than 5 minutes."
								: "Offline for more than 5 minutes.",
						since: phone.since,
					});
				continue;
			}
			const hot = this.hot.get(phone.serial);
			if (hot)
				alerts.push({
					key: `battery_hot:${phone.serial}`,
					kind: "battery_hot",
					serial: phone.serial,
					severity: "critical",
					message: `Battery at ${(hot.temperatureDeciC / 10).toFixed(1)} °C (over 45 °C).`,
					since: hot.since,
				});
		}
		for (const [key, { since }] of this.outOfService) {
			if (nowMs - since <= OUT_OF_SERVICE_ALERT_MS) continue;
			const [serial = "", slot = "0"] = key.split(":");
			alerts.push({
				key: `out_of_service:${key}`,
				kind: "out_of_service",
				serial,
				severity: "warning",
				message: `SIM ${Number(slot) + 1} out of service for more than 30 minutes.`,
				since: new Date(since).toISOString(),
			});
		}
		return alerts;
	}

	private write(
		serial: string,
		bucket: Bucket,
		mobile: { rx: number; tx: number } | null,
	): void {
		const phoneId = this.db
			.query<{ id: number }, [string]>("SELECT id FROM phones WHERE serial = ?")
			.get(serial)?.id;
		if (phoneId === undefined || bucket.samples === 0) return;
		const signal: Record<string, { minLevel: number; dbm: number | null }> = {};
		const service: Record<string, number> = {};
		for (const [slot, value] of bucket.slots) {
			signal[slot] = {
				minLevel: value.minLevel,
				dbm:
					value.dbmCount > 0 ? Math.round(value.dbmSum / value.dbmCount) : null,
			};
			service[slot] = Math.round((value.inService / value.samples) * 100) / 100;
		}
		this.db.run(
			`INSERT OR REPLACE INTO health_5m
				(phone_id, bucket, samples, battery_pct, battery_temp_dc, signal_json, service_json, mobile_rx, mobile_tx)
			VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)`,
			[
				phoneId,
				new Date(bucket.start).toISOString(),
				bucket.samples,
				bucket.batteryCount > 0
					? Math.round(bucket.batterySum / bucket.batteryCount)
					: null,
				bucket.tempMax,
				JSON.stringify(signal),
				JSON.stringify(service),
				mobile?.rx ?? null,
				mobile?.tx ?? null,
			],
		);
	}
}
