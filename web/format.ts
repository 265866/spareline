// +15125550101 -> +1 (512) 555-0101; other countries stay as stored.
export function formatNumber(e164: string | null): string {
	if (!e164) return "Unknown number";
	const us = /^\+1(\d{3})(\d{3})(\d{4})$/.exec(e164);
	return us ? `+1 (${us[1]}) ${us[2]}-${us[3]}` : e164;
}

export function ago(iso: string | null, now = Date.now()): string {
	if (!iso) return "never";
	const seconds = Math.max(0, Math.round((now - Date.parse(iso)) / 1000));
	if (seconds < 45) return "just now";
	const minutes = Math.round(seconds / 60);
	if (minutes < 60) return `${minutes} min ago`;
	const hours = Math.round(minutes / 60);
	if (hours < 36) return `${hours} h ago`;
	return `${Math.round(hours / 24)} d ago`;
}

export const SERVICE_LABEL: Record<string, string> = {
	in_service: "In service",
	out_of_service: "No service",
	emergency_only: "Emergency only",
	power_off: "Radio off",
	unknown: "Unknown",
};

export const EVENT_LABEL: Record<string, string> = {
	"phone.added": "New phone",
	"phone.online": "Online",
	"phone.offline": "Offline",
	"phone.needs_auth": "Needs USB debugging approval",
	"phone.no_permissions": "No USB permission on the host",
	"phone.other_state": "Unusual adb state",
	"sim.added": "New SIM",
	"sim.placed": "SIM inserted",
	"sim.removed": "SIM removed",
	"number.bound": "Number linked",
	"number.detected": "Number detected",
	"number.changed": "Number changed",
	"adb.unreachable": "adb server unreachable",
	"adb.reachable": "adb server back",
};
