// `bun run dev`: builds the web UI, rebuilds it when web/ changes, and runs
// the server. `bun --watch` restarts this whole script when server code
// changes. Reload the browser after a web change.
import { watch } from "node:fs";
import { join } from "node:path";
import { buildWeb } from "./build-web";

await buildWeb();
let timer: Timer | undefined;
watch(join(import.meta.dir, "..", "web"), { recursive: true }, () => {
	clearTimeout(timer);
	timer = setTimeout(() => {
		buildWeb().catch((error: unknown) => console.error(error));
	}, 100);
});
await import("../src/server");
