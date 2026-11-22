import { Copy, KeyRound, Trash2 } from "lucide-react";
import { type FormEvent, useState } from "react";
import { type ApiKeyView, api, useResource } from "../api";
import { ago } from "../format";

// Creating, listing, and revoking API keys. A new key is shown once.
export function ApiKeys() {
	const keys = useResource<{ keys: ApiKeyView[] }>("/api-keys", 60_000);
	const [name, setName] = useState("");
	const [scope, setScope] = useState<"read" | "full">("read");
	const [created, setCreated] = useState<{ key: string; name: string } | null>(
		null,
	);
	const [error, setError] = useState<string | null>(null);
	const [busy, setBusy] = useState(false);

	const create = async (event: FormEvent) => {
		event.preventDefault();
		setBusy(true);
		setError(null);
		try {
			const result = await api<{ key: string; apiKey: ApiKeyView }>(
				"/api-keys",
				{ method: "POST", body: { name, scope } },
			);
			setCreated({ key: result.key, name: result.apiKey.name });
			setName("");
			keys.reload();
		} catch (reason) {
			setError(reason instanceof Error ? reason.message : String(reason));
		} finally {
			setBusy(false);
		}
	};

	const revoke = async (key: ApiKeyView) => {
		if (
			!window.confirm(
				`Revoke "${key.name}" (${key.prefix}…)? Scripts using it stop working at once.`,
			)
		)
			return;
		try {
			await api(`/api-keys/${key.id}`, { method: "DELETE" });
			keys.reload();
		} catch (reason) {
			setError(reason instanceof Error ? reason.message : String(reason));
		}
	};

	const active = keys.data?.keys.filter((key) => !key.revokedAt) ?? [];
	return (
		<section className="flex flex-col gap-3 rounded-xl border bg-white p-4 dark:bg-zinc-900">
			<div>
				<h2 className="font-medium">API keys</h2>
				<p className="text-sm text-zinc-500 dark:text-zinc-400">
					For scripts: send a key as{" "}
					<code className="font-mono">Authorization: Bearer &lt;key&gt;</code>.
					A read key can read texts, events, and the inventory; a full key can
					also edit, set phones up, and open screens. Keys cannot manage keys or
					passwords.
				</p>
			</div>
			{created && (
				<div
					role="status"
					className="flex flex-col gap-2 rounded-lg bg-emerald-50 p-3 text-sm text-emerald-900 dark:bg-emerald-950/60 dark:text-emerald-200"
				>
					<span>
						Key "{created.name}" created. Copy it now: it is not shown again.
					</span>
					<span className="flex items-center gap-2">
						<code className="break-all font-mono text-xs">{created.key}</code>
						<button
							type="button"
							title="Copy the key"
							onClick={() => void navigator.clipboard.writeText(created.key)}
							className="shrink-0 rounded-md border p-1 hover:bg-white/60 dark:hover:bg-zinc-800"
						>
							<Copy size={14} />
						</button>
					</span>
				</div>
			)}
			<form onSubmit={create} className="flex flex-wrap items-end gap-2">
				<label className="flex flex-col gap-1 text-sm">
					Name
					<input
						required
						maxLength={64}
						value={name}
						onChange={(event) => setName(event.target.value)}
						placeholder="What uses it"
						className="w-56 rounded-md border bg-transparent px-3 py-1.5 outline-none focus:border-sky-500"
					/>
				</label>
				<label className="flex flex-col gap-1 text-sm">
					Access
					<select
						value={scope}
						onChange={(event) =>
							setScope(event.target.value as "read" | "full")
						}
						className="rounded-md border bg-transparent px-2 py-1.5"
					>
						<option value="read">Read only</option>
						<option value="full">Full</option>
					</select>
				</label>
				<button
					type="submit"
					disabled={busy}
					className="inline-flex items-center gap-1.5 rounded-md bg-zinc-900 px-3 py-1.5 text-sm font-medium text-white disabled:opacity-50 dark:bg-zinc-100 dark:text-zinc-900"
				>
					<KeyRound size={14} /> Create key
				</button>
			</form>
			{error && (
				<p role="alert" className="text-sm text-red-600">
					{error}
				</p>
			)}
			{keys.error && (
				<p className="text-sm text-red-600">
					Could not load keys: {keys.error}
				</p>
			)}
			{active.length > 0 && (
				<ul className="divide-y rounded-lg border text-sm">
					{active.map((key) => (
						<li
							key={key.id}
							className="flex flex-wrap items-center gap-x-3 gap-y-1 px-3 py-2"
						>
							<span className="font-medium">{key.name}</span>
							<code className="font-mono text-xs text-zinc-500">
								{key.prefix}…
							</code>
							<span className="text-xs text-zinc-500">
								{key.scope === "read" ? "read only" : "full"} · {key.user} ·{" "}
								{key.lastUsedAt ? `used ${ago(key.lastUsedAt)}` : "never used"}
							</span>
							<button
								type="button"
								onClick={() => void revoke(key)}
								title="Revoke this key"
								className="ml-auto inline-flex items-center gap-1 rounded-md border px-2 py-1 text-xs hover:bg-zinc-50 dark:hover:bg-zinc-800"
							>
								<Trash2 size={12} /> Revoke
							</button>
						</li>
					))}
				</ul>
			)}
		</section>
	);
}
