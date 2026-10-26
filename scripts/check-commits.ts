// Fails unless every commit subject in a range, and the PR title in PR_TITLE
// when set, follows Conventional Commits: `type(scope)!: subject`.
//
//   bun scripts/check-commits.ts BASE..HEAD
//
// Merge commits are skipped; GitHub titles them after the PR, which this
// checks through PR_TITLE.

const TYPES = [
	"build",
	"chore",
	"ci",
	"docs",
	"feat",
	"fix",
	"perf",
	"refactor",
	"revert",
	"style",
	"test",
];
const CONVENTIONAL = new RegExp(
	`^(${TYPES.join("|")})(\\([a-z0-9-]+\\))?!?: \\S.{0,99}$`,
);

export function conventional(subject: string): boolean {
	return CONVENTIONAL.test(subject);
}

if (import.meta.main) {
	const range = Bun.argv[2];
	if (!range) {
		console.error("usage: bun scripts/check-commits.ts BASE..HEAD");
		process.exit(2);
	}
	const proc = Bun.spawn(
		["git", "log", "--no-merges", "--format=%h %s", range],
		{ stdout: "pipe" },
	);
	const log = await new Response(proc.stdout).text();
	if ((await proc.exited) !== 0) process.exit(1);
	const bad = log
		.split("\n")
		.filter(Boolean)
		.filter((line) => !conventional(line.slice(line.indexOf(" ") + 1)));
	const title = process.env.PR_TITLE;
	if (title !== undefined && !conventional(title))
		bad.push(`PR title: ${title}`);
	for (const line of bad) console.error(`not a conventional commit: ${line}`);
	if (bad.length > 0) {
		console.error(`Use one of: ${TYPES.join(", ")}. Example: feat(sms): ...`);
		process.exit(1);
	}
	console.log("Commit subjects and title are conventional.");
}
