import { type FormEvent, useState } from "react";
import { api } from "../api";
import { ApiKeys } from "../components/keys";

export function SettingsPage({ name }: { name: string }) {
	return (
		<div className="flex max-w-xl flex-col gap-6">
			<h1 className="text-xl font-semibold">Settings</h1>
			<PasswordForm name={name} />
			<ApiKeys />
		</div>
	);
}

function PasswordForm({ name }: { name: string }) {
	const [current, setCurrent] = useState("");
	const [next, setNext] = useState("");
	const [result, setResult] = useState<{ ok: boolean; text: string } | null>(
		null,
	);
	const [busy, setBusy] = useState(false);

	const submit = async (event: FormEvent) => {
		event.preventDefault();
		setBusy(true);
		setResult(null);
		try {
			await api("/auth/password", {
				method: "POST",
				body: { current, next },
			});
			setCurrent("");
			setNext("");
			setResult({
				ok: true,
				text: "Password changed. Other browsers were signed out.",
			});
		} catch (reason) {
			setResult({
				ok: false,
				text: reason instanceof Error ? reason.message : String(reason),
			});
		} finally {
			setBusy(false);
		}
	};

	return (
		<form
			onSubmit={submit}
			className="flex flex-col gap-3 rounded-xl border bg-white p-4 dark:bg-zinc-900"
		>
			<h2 className="font-medium">Password for {name}</h2>
			<input
				type="text"
				name="username"
				autoComplete="username"
				value={name}
				readOnly
				hidden
			/>
			<label className="flex flex-col gap-1 text-sm">
				Current password
				<input
					type="password"
					autoComplete="current-password"
					required
					value={current}
					onChange={(event) => setCurrent(event.target.value)}
					className="rounded-md border bg-transparent px-3 py-2 outline-none focus:border-sky-500"
				/>
			</label>
			<label className="flex flex-col gap-1 text-sm">
				New password (at least 12 characters)
				<input
					type="password"
					autoComplete="new-password"
					required
					minLength={12}
					value={next}
					onChange={(event) => setNext(event.target.value)}
					className="rounded-md border bg-transparent px-3 py-2 outline-none focus:border-sky-500"
				/>
			</label>
			{result && (
				<p
					role="status"
					className={`text-sm ${result.ok ? "text-emerald-700 dark:text-emerald-400" : "text-red-600"}`}
				>
					{result.text}
				</p>
			)}
			<button
				type="submit"
				disabled={busy}
				className="self-start rounded-md bg-zinc-900 px-3 py-1.5 text-sm font-medium text-white disabled:opacity-50 dark:bg-zinc-100 dark:text-zinc-900"
			>
				Change password
			</button>
		</form>
	);
}
