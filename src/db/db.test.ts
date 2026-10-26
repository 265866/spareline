import { afterEach, expect, test } from "bun:test";
import {
	cpSync,
	existsSync,
	mkdtempSync,
	readdirSync,
	rmSync,
	writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { backupDatabase } from "./backup";
import { listMigrations, openDb } from "./db";

const dirs: string[] = [];
const tempDir = () => {
	const dir = mkdtempSync(join(tmpdir(), "spareline-db-"));
	dirs.push(dir);
	return dir;
};
afterEach(() => {
	for (const dir of dirs.splice(0))
		rmSync(dir, { recursive: true, force: true });
});

const EXPECTED_TABLES = [
	"device_events",
	"inventory_edits",
	"number_bindings",
	"numbers",
	"phones",
	"sim_placements",
	"sims",
];

test("a new database gets every table, WAL, and foreign keys", () => {
	const db = openDb(join(tempDir(), "data", "spareline.db"));
	const tables = db
		.query<{ name: string }, []>(
			"SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND name != '_migrations' ORDER BY name",
		)
		.all()
		.map((row) => row.name);
	expect(tables).toEqual(EXPECTED_TABLES);
	expect(db.query("PRAGMA journal_mode").get()).toEqual({
		journal_mode: "wal",
	});
	expect(db.query("PRAGMA foreign_keys").get()).toEqual({ foreign_keys: 1 });
	// NORMAL is 1.
	expect(db.query("PRAGMA synchronous").get()).toEqual({ synchronous: 1 });
	db.close();
});

test("reopening applies nothing twice", () => {
	const path = join(tempDir(), "spareline.db");
	openDb(path).close();
	const db = openDb(path);
	expect(db.query("SELECT id, file FROM _migrations").all()).toEqual(
		listMigrations(join(import.meta.dir, "migrations")),
	);
	db.close();
});

test("a later migration snapshots the existing database first", () => {
	const dir = tempDir();
	const migrations = join(dir, "migrations");
	const real = listMigrations(join(import.meta.dir, "migrations"));
	cpSync(join(import.meta.dir, "migrations"), migrations, { recursive: true });
	const path = join(dir, "spareline.db");
	const first = openDb(path, migrations);
	first.run(
		"INSERT INTO numbers (e164_source, note, created_at) VALUES ('none', 'Facebook ad', '2026-10-21')",
	);
	first.close();

	const next = String((real.at(-1)?.id ?? 0) + 1).padStart(3, "0");
	writeFileSync(
		join(migrations, `${next}_add_number_order.sql`),
		"ALTER TABLE numbers ADD COLUMN sort INTEGER;",
	);
	const db = openDb(path, migrations);
	expect(db.query("SELECT note, sort FROM numbers").all()).toEqual([
		{ note: "Facebook ad", sort: null },
	]);
	db.close();
	const backups = readdirSync(join(dir, "backups"));
	expect(backups).toHaveLength(1);
	expect(backups[0]).toStartWith(`pre-migration-${next}-`);
});

test("a new database is not snapshotted, and an existing one once per run", () => {
	const dir = tempDir();
	const path = join(dir, "spareline.db");
	writeFileSync(join(dir, "001_a.sql"), "CREATE TABLE a (x);");
	writeFileSync(join(dir, "020_c.sql"), "CREATE TABLE c (x);");
	openDb(path, dir).close();
	expect(existsSync(join(dir, "backups"))).toBe(false);

	writeFileSync(join(dir, "010_b.sql"), "CREATE TABLE b (x);");
	writeFileSync(join(dir, "030_d.sql"), "CREATE TABLE d (x);");
	const db = openDb(path, dir);
	expect(db.query("SELECT id FROM _migrations ORDER BY id").all()).toEqual([
		{ id: 1 },
		{ id: 10 },
		{ id: 20 },
		{ id: 30 },
	]);
	db.close();
	const backups = readdirSync(join(dir, "backups"));
	expect(backups).toHaveLength(1);
	expect(backups[0]).toStartWith("pre-migration-010-");
});

test("migration numbers may skip ranges but must be unique", () => {
	const dir = tempDir();
	writeFileSync(join(dir, "001_init.sql"), "");
	writeFileSync(join(dir, "010_sms.sql"), "");
	writeFileSync(join(dir, "020_screens.sql"), "");
	expect(listMigrations(dir).map((migration) => migration.id)).toEqual([
		1, 10, 20,
	]);
	writeFileSync(join(dir, "010_other.sql"), "");
	expect(() => listMigrations(dir)).toThrow("conflict");
});

test("constraints hold: one open placement per slot, numbers are E.164", () => {
	const db = openDb(":memory:");
	const now = "2026-10-21T00:00:00.000Z";
	db.run(
		"INSERT INTO phones (serial, site, last_state, first_seen_at) VALUES ('A', 'home', 'device', ?)",
		[now],
	);
	db.run(
		"INSERT INTO sims (iccid, first_seen_at, last_seen_at) VALUES ('8901', ?, ?), ('8902', ?, ?)",
		[now, now, now, now],
	);
	db.run(
		"INSERT INTO sim_placements (sim_id, phone_id, slot, sub_id, from_at) VALUES (1, 1, 0, 1, ?)",
		[now],
	);
	expect(() =>
		db.run(
			"INSERT INTO sim_placements (sim_id, phone_id, slot, sub_id, from_at) VALUES (2, 1, 0, 2, ?)",
			[now],
		),
	).toThrow("UNIQUE");
	expect(() =>
		db.run(
			"INSERT INTO numbers (e164, e164_source, created_at) VALUES ('5125550101', 'detected', ?)",
			[now],
		),
	).toThrow("CHECK");
	expect(() =>
		db.run(
			"INSERT INTO sim_placements (sim_id, phone_id, slot, sub_id, from_at) VALUES (9, 1, 1, 1, ?)",
			[now],
		),
	).toThrow("FOREIGN KEY");
	db.close();
});

test("backups are consistent copies that pass integrity_check", () => {
	const dir = tempDir();
	const path = join(dir, "spareline.db");
	const db = openDb(path);
	db.run(
		"INSERT INTO numbers (e164_source, note, created_at) VALUES ('none', 'Craigslist', '2026-10-21')",
	);
	const target = join(dir, "backup.db");
	backupDatabase(path, target);
	db.close();
	const copy = openDb(target);
	expect(copy.query("SELECT note FROM numbers").all()).toEqual([
		{ note: "Craigslist" },
	]);
	copy.close();
	expect(() => backupDatabase(path, target)).toThrow("already exists");
	expect(existsSync(target)).toBe(true);
});
