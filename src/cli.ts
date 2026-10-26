// Manages who can sign in, on the server itself:
//
//   bun src/cli.ts users add NAME             prints a generated password
//   bun src/cli.ts users list
//   bun src/cli.ts users reset-password NAME  prints a new one, signs out
//   bun src/cli.ts users remove NAME
//
// It opens the database in DATA_DIR, like the server, which may keep running.
// In the container: podman exec spareline bun src/cli.ts users add NAME
import { join } from "node:path";
import { Auth, AuthError } from "./auth/auth";
import { openDb } from "./db/db";
import { parseEnv } from "./env";

const USAGE = `usage:
  bun src/cli.ts users add NAME
  bun src/cli.ts users list
  bun src/cli.ts users reset-password NAME
  bun src/cli.ts users remove NAME`;

export async function run(
	args: string[],
	auth: Auth,
	print: (line: string) => void,
): Promise<number> {
	const [group, command, name] = args;
	if (group !== "users") {
		print(USAGE);
		return 2;
	}
	try {
		if (command === "list" && !name) {
			const users = auth.listUsers();
			if (users.length === 0) print("No users yet.");
			for (const user of users) print(`${user.name}\tsince ${user.createdAt}`);
			return 0;
		}
		if (command === "add" && name) {
			const { user, password } = await auth.addUser(name);
			print(`Added ${user.name}. Password (shown once): ${password}`);
			return 0;
		}
		if (command === "reset-password" && name) {
			const password = await auth.resetPassword(name);
			print(
				`New password for ${name} (shown once; every session ended): ${password}`,
			);
			return 0;
		}
		if (command === "remove" && name) {
			auth.removeUser(name);
			print(`Removed ${name} and ended their sessions.`);
			return 0;
		}
	} catch (error) {
		if (!(error instanceof AuthError)) throw error;
		print(error.message);
		return 1;
	}
	print(USAGE);
	return 2;
}

if (import.meta.main) {
	let dataDir: string;
	try {
		dataDir = parseEnv(process.env).DATA_DIR;
	} catch (error) {
		console.error(error instanceof Error ? error.message : String(error));
		process.exit(1);
	}
	const db = openDb(join(dataDir, "spareline.db"));
	const code = await run(Bun.argv.slice(2), new Auth(db), (line) =>
		console.log(line),
	);
	db.close();
	process.exit(code);
}
