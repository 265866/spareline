// Bundles the UI into dist/: index.html plus content-hashed assets under
// /assets/, which the server caches forever.
import { rmSync } from "node:fs";
import { join } from "node:path";
import tailwind from "bun-plugin-tailwind";

const root = join(import.meta.dir, "..");
const outdir = join(root, "dist");

// Tailwind looks for class names in the files under the working directory,
// so a build started elsewhere, such as the repository root, would pick up
// words from the Rust sources.
process.chdir(root);
rmSync(outdir, { recursive: true, force: true });
const result = await Bun.build({
	entrypoints: [join(root, "index.html")],
	outdir,
	target: "browser",
	plugins: [tailwind],
	minify: true,
	sourcemap: "linked",
	naming: {
		entry: "[name].[ext]",
		chunk: "assets/[name]-[hash].[ext]",
		asset: "assets/[name]-[hash].[ext]",
	},
	publicPath: "/",
	// Without it React ships its development build.
	define: { "process.env.NODE_ENV": JSON.stringify("production") },
});
if (!result.success)
	throw new AggregateError(result.logs, "the web UI did not build");
for (const output of result.outputs)
	console.log(
		`${output.path.slice(outdir.length + 1).padEnd(40)} ${(output.size / 1024).toFixed(1)} KiB`,
	);
