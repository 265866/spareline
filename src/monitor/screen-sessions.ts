import type { ScreenSessionInfo } from "../screens/manager";

// Whether a phone has a scrcpy session, which holds stay_on_while_plugged_in
// until it ends. An "idle" entry has no scrcpy process left.
export function screenSessionActive(
	list: () => Pick<ScreenSessionInfo, "serial" | "state">[],
): (serial: string) => boolean {
	return (serial) =>
		list().some(
			(session) => session.serial === serial && session.state !== "idle",
		);
}
