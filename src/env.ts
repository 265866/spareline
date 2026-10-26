import { z } from "zod";
import { parseEndpoint } from "./adb/client";

const Env = z.object({
	HOST: z.string().default("127.0.0.1"),
	PORT: z.coerce.number().int().min(1).max(65535).default(8740),
	ADB_SERVER: z
		.string()
		.default("127.0.0.1:5037")
		.transform((value, context) => {
			try {
				return parseEndpoint(value);
			} catch (error) {
				context.addIssue({ code: "custom", message: String(error) });
				return z.NEVER;
			}
		}),
	DATA_DIR: z.string().default("data"),
	// Where this instance runs; stored on every phone for a later multi-host view.
	SITE: z.string().trim().min(1).default("local"),
	// Seconds between health probes of each online phone.
	HEALTH_INTERVAL_S: z.coerce.number().int().min(5).max(3600).default(60),
	WEB_DIR: z.string().default("dist/web"),
	// Seconds between phone policy (drift) checks of each online phone.
	POLICY_CHECK_INTERVAL_S: z.coerce
		.number()
		.int()
		.min(30)
		.max(86_400)
		.default(300),
	// Non-IMS mobile bytes per SIM in 24 h that turn data off again and the
	// phone red.
	DATA_THRESHOLD_BYTES: z.coerce.number().int().min(0).default(1_000_000),
});

type Config = z.infer<typeof Env>;

export function parseEnv(env: Record<string, string | undefined>): Config {
	const result = Env.safeParse(env);
	if (!result.success)
		throw new Error(`invalid environment:\n${z.prettifyError(result.error)}`);
	return result.data;
}
