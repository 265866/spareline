import { Database } from "bun:sqlite";
import {
	closeSync,
	fsyncSync,
	linkSync,
	lstatSync,
	mkdtempSync,
	openSync,
	rmSync,
	statSync,
} from "node:fs";
import { basename, dirname, join, resolve } from "node:path";

// Copies a live database to `destination` with VACUUM INTO, which reads one
// consistent snapshot while the app keeps writing, checks the copy with
// PRAGMA integrity_check, and publishes it atomically. Refuses to overwrite.
export function backupDatabase(source: string, destination: string): void {
	const sourcePath = resolve(source);
	const destinationPath = resolve(destination);
	if (!statSync(sourcePath).isFile())
		throw new Error(`backup source is not a file: ${sourcePath}`);
	if (lstatSync(destinationPath, { throwIfNoEntry: false }))
		throw new Error(`backup destination already exists: ${destinationPath}`);

	const work = mkdtempSync(
		join(dirname(destinationPath), `.${basename(destinationPath)}-`),
	);
	const temporary = join(work, "snapshot.db");
	try {
		const live = new Database(sourcePath, { readonly: true });
		try {
			live.run("PRAGMA busy_timeout = 30000");
			live.run("VACUUM INTO ?", [temporary]);
		} finally {
			live.close();
		}
		checkIntegrity(temporary);
		const file = openSync(temporary, "r+");
		try {
			fsyncSync(file);
		} finally {
			closeSync(file);
		}
		// A hard link publishes the finished file at once and fails if the
		// destination appeared meanwhile.
		linkSync(temporary, destinationPath);
		if (process.platform !== "win32") {
			const directory = openSync(dirname(destinationPath), "r");
			try {
				fsyncSync(directory);
			} finally {
				closeSync(directory);
			}
		}
	} finally {
		rmSync(work, { recursive: true, force: true });
	}
}

function checkIntegrity(path: string): void {
	const db = new Database(path, { readonly: true });
	try {
		const rows = db
			.query<{ integrity_check: string }, []>("PRAGMA integrity_check")
			.all();
		if (rows.length !== 1 || rows[0]?.integrity_check !== "ok")
			throw new Error(
				`integrity_check failed for ${path}: ${JSON.stringify(rows)}`,
			);
	} finally {
		db.close();
	}
}

// bun src/db/backup.ts SOURCE DESTINATION   (used by deploy/podman/*.sh)
// bun src/db/backup.ts --check FILE
if (import.meta.main) {
	const args = Bun.argv.slice(2);
	try {
		if (args[0] === "--check" && args[1] && args.length === 2) {
			checkIntegrity(args[1]);
			console.log(`integrity ok: ${resolve(args[1])}`);
		} else if (args.length === 2 && args[0] && args[1]) {
			backupDatabase(args[0], args[1]);
			console.log(`snapshot saved to ${resolve(args[1])}`);
		} else {
			console.error(
				"usage: bun src/db/backup.ts SOURCE DESTINATION | --check FILE",
			);
			process.exitCode = 2;
		}
	} catch (error) {
		console.error(error instanceof Error ? error.message : String(error));
		process.exitCode = 1;
	}
}
