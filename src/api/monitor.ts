import { Elysia } from "elysia";
import { ConflictError } from "../inventory/store";
import { type Monitor, MonitorError } from "../monitor/monitor";
import { CheckReport, ProvisionReport } from "./monitor-schema";
import { ApiError } from "./schema";

export const ACTION_HEADER = "x-spareline";

// Phone-changing requests must be ones a page on another site cannot make:
// a browser sends a cross-site form only as form or text content, and a
// JSON body or a custom header from another origin needs a CORS preflight
// that this server never grants. Sec-Fetch-Site rejects the rest.
function sameSiteOnly({
	request,
	set,
}: {
	request: Request;
	set: { status?: number | string };
}) {
	const headers = request.headers;
	const json = (headers.get("content-type") ?? "").startsWith(
		"application/json",
	);
	if (
		headers.get("sec-fetch-site") === "cross-site" ||
		!(json || headers.get(ACTION_HEADER) === "1")
	) {
		set.status = 403;
		return {
			error: `send Content-Type: application/json or ${ACTION_HEADER}: 1, from this site`,
		};
	}
}

// Set-up and an on-demand drift check. Mounted under /api/v1.
export function monitorRoutes(monitor: Monitor | null) {
	const routes = new Elysia({ name: "monitor-routes" });
	if (!monitor) return routes;
	const conflict = async <T>(work: () => Promise<T>): Promise<T> => {
		try {
			return await work();
		} catch (error) {
			if (error instanceof MonitorError) throw new ConflictError(error.message);
			throw error;
		}
	};
	return routes
		.post(
			"/phones/:serial/provision",
			({ params }) => conflict(() => monitor.provision(params.serial)),
			{
				beforeHandle: sameSiteOnly,
				response: { 200: ProvisionReport, 403: ApiError, 409: ApiError },
				detail: {
					summary: "Set a phone up to match the policy",
					description:
						"Applies every rule of the policy profile that adb can set (such as radios and data off, Data Saver, the default SMS app, screen settings, and removing the listed apps, which deletes their data), then re-reads and verifies. Idempotent: a phone that already complies gets no commands. Answers 409 without a profile. Requires Content-Type: application/json (an empty `{}` body is fine) or `X-Spareline: 1`.",
					tags: ["status"],
				},
			},
		)
		.post(
			"/phones/:serial/check",
			({ params }) => conflict(() => monitor.check(params.serial)),
			{
				beforeHandle: sameSiteOnly,
				response: { 200: CheckReport, 403: ApiError, 409: ApiError },
				detail: {
					summary: "Run the drift check now",
					description:
						"One batched read of every rule and the data watch. Radio and data drift is reverted and logged as a `drift` event. A failed check is reported in `error`. Same request requirements as provision.",
					tags: ["status"],
				},
			},
		);
}
