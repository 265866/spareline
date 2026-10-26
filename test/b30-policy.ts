import { readFileSync } from "node:fs";
import { join } from "node:path";
import { loadPolicy, logBufferCommand, Profile } from "../src/monitor/policy";

// The example profile for the NUU B30 the fixtures were recorded on, which
// the monitor tests run with.
const PATH = join(import.meta.dir, "../profiles/nuu-b30-no-data.json");
export const B30_PROFILE = Profile.parse(
	JSON.parse(readFileSync(PATH, "utf8")),
);
export const B30_POLICY = loadPolicy(PATH);
export const LOG_BUFFER_COMMAND = logBufferCommand(8);
