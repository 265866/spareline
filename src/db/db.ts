import { Database } from "bun:sqlite";
import { mkdirSync, readdirSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";

const MIGRATIONS_DIR = join(import.meta.dir, "migrations");

// Opens the database and applies pending migrations. WAL with
// synchronous=NORMAL keeps SD card writes low: a commit is durable once the
// WAL is synced at checkpoint, and a power cut loses at most the last few
// transactions, never consistency.
export function openDb(path: string, migrationsDir = MIGRATIONS_DIR): Database {
	if (path !== ":memory:")
		mkdirSync(dirname(resolve(path)), { recursive: true });
	const db = new Database(path, { strict: true, create: true });
	db.run("PRAGMA journal_mode = WAL");
	db.run("PRAGMA synchronous = NORMAL");
	db.run("PRAGMA foreign_keys = ON");
	db.run("PRAGMA busy_timeout = 5000");
	runMigrations(db, migrationsDir, path === ":memory:" ? null : resolve(path));
	return db;
}

export type Migration = { id: number; file: string };

// Every NNN_name.sql file, in ascending order. Numbers may skip (each later
// feature owns a range, such as 010-019) but must be unique, so two files
// claiming one number fail loudly at startup.
export function listMigrations(dir: string): Migration[] {
	const migrations = readdirSync(dir)
		.filter((file) => file.endsWith(".sql"))
		.map((file) => {
			const match = /^(\d{3})_[a-z0-9_]+\.sql$/.exec(file);
			if (!match)
				throw new Error(`migration file name must be NNN_name.sql: ${file}`);
			return { id: Number(match[1]), file };
		})
		.sort((a, b) => a.id - b.id);
	migrations.forEach((migration, index) => {
		const previous = migrations[index - 1];
		if (migration.id === 0 || (previous && previous.id >= migration.id))
			throw new Error(
				`migration numbers must be unique and start at 001; ${previous?.file ?? "(none)"} and ${migration.file} conflict`,
			);
	});
	return migrations;
}

function runMigrations(db: Database, dir: string, dbPath: string | null): void {
	db.run(
		"CREATE TABLE IF NOT EXISTS _migrations (id INTEGER PRIMARY KEY, file TEXT NOT NULL, applied_at TEXT NOT NULL)",
	);
	const applied = new Set(
		db
			.query<{ id: number }, []>("SELECT id FROM _migrations")
			.all()
			.map((row) => row.id),
	);
	// Only a database that existed before this run holds anything to undo,
	// and one snapshot, before its first pending migration, undoes the run.
	let snapshotDue = dbPath !== null && applied.size > 0;
	for (const migration of listMigrations(dir)) {
		if (applied.has(migration.id)) continue;
		if (dbPath && snapshotDue) {
			snapshot(db, dbPath, migration);
			snapshotDue = false;
		}
		const sql = readFileSync(join(dir, migration.file), "utf8");
		db.transaction(() => {
			db.run(sql);
			db.run(
				"INSERT INTO _migrations (id, file, applied_at) VALUES (?, ?, ?)",
				[migration.id, migration.file, new Date().toISOString()],
			);
		})();
		applied.add(migration.id);
		console.error(
			JSON.stringify({
				at: new Date().toISOString(),
				event: "migration-applied",
				file: migration.file,
			}),
		);
	}
}

function snapshot(db: Database, dbPath: string, migration: Migration): void {
	const dir = join(dirname(dbPath), "backups");
	mkdirSync(dir, { recursive: true });
	const stamp = new Date().toISOString().replace(/[:.]/g, "-");
	const target = join(
		dir,
		`pre-migration-${String(migration.id).padStart(3, "0")}-${stamp}.db`,
	);
	db.run("VACUUM INTO ?", [target]);
}
