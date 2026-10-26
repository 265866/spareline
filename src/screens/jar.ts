import { join } from "node:path";

// scrcpy-server v3.3.3 from https://github.com/Genymobile/scrcpy/releases/tag/v3.3.3,
// vendored with its license. The pin matches the release's published digest.
export const SERVER_JAR_PATH = join(
	import.meta.dir,
	"../../vendor/scrcpy-server-v3.3.3/scrcpy-server-v3.3.3",
);
export const SERVER_JAR_SHA256 =
	"7e70323ba7f259649dd4acce97ac4fefbae8102b2c6d91e2e7be613fd5354be0";

// Reads the vendored jar and refuses one whose hash differs from the pin.
export async function loadServerJar(
	path = SERVER_JAR_PATH,
): Promise<Uint8Array> {
	const file = Bun.file(path);
	if (!(await file.exists()))
		throw new Error(`scrcpy-server jar missing at ${path}`);
	const jar = new Uint8Array(await file.arrayBuffer());
	const digest = new Bun.CryptoHasher("sha256").update(jar).digest("hex");
	if (digest !== SERVER_JAR_SHA256)
		throw new Error(
			`scrcpy-server jar at ${path} has sha256 ${digest}, expected ${SERVER_JAR_SHA256}`,
		);
	return jar;
}
