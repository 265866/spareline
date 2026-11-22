import { LogIn } from "lucide-react";
import { type FormEvent, useState } from "react";

export function LoginPage({
	noUsers,
	onSignIn,
}: {
	noUsers: boolean;
	onSignIn: (name: string, password: string) => Promise<void>;
}) {
	const [name, setName] = useState("");
	const [password, setPassword] = useState("");
	const [error, setError] = useState<string | null>(null);
	const [busy, setBusy] = useState(false);

	const submit = async (event: FormEvent) => {
		event.preventDefault();
		setBusy(true);
		setError(null);
		try {
			await onSignIn(name, password);
		} catch (reason) {
			setError(reason instanceof Error ? reason.message : String(reason));
			setBusy(false);
		}
	};

	return (
		<main className="flex min-h-screen items-center justify-center px-4">
			<form
				onSubmit={submit}
				className="flex w-full max-w-sm flex-col gap-4 rounded-xl border bg-white p-6 shadow-sm dark:bg-zinc-900"
			>
				<div>
					<h1 className="text-lg font-semibold">spareline</h1>
					<p className="text-sm text-zinc-500 dark:text-zinc-400">
						Sign in to see your phones and their texts.
					</p>
				</div>
				{noUsers && (
					<p className="rounded-lg bg-amber-50 p-3 text-sm text-amber-900 dark:bg-amber-950/60 dark:text-amber-200">
						Nobody can sign in yet. On the server, run{" "}
						<code className="font-mono">bun src/cli.ts users add NAME</code> (in
						the container:{" "}
						<code className="font-mono">
							podman exec spareline bun src/cli.ts users add NAME
						</code>
						) and sign in with the password it prints.
					</p>
				)}
				<label className="flex flex-col gap-1 text-sm">
					Name
					<input
						name="username"
						autoComplete="username"
						required
						value={name}
						onChange={(event) => setName(event.target.value)}
						className="rounded-md border bg-transparent px-3 py-2 outline-none focus:border-sky-500"
					/>
				</label>
				<label className="flex flex-col gap-1 text-sm">
					Password
					<input
						name="password"
						type="password"
						autoComplete="current-password"
						required
						value={password}
						onChange={(event) => setPassword(event.target.value)}
						className="rounded-md border bg-transparent px-3 py-2 outline-none focus:border-sky-500"
					/>
				</label>
				{error && (
					<p role="alert" className="text-sm text-red-600">
						{error}
					</p>
				)}
				<button
					type="submit"
					disabled={busy}
					className="inline-flex items-center justify-center gap-2 rounded-md bg-zinc-900 px-3 py-2 text-sm font-medium text-white disabled:opacity-50 dark:bg-zinc-100 dark:text-zinc-900"
				>
					<LogIn size={15} /> Sign in
				</button>
			</form>
		</main>
	);
}
