import { Check, Pencil, X } from "lucide-react";
import { type ReactNode, useEffect, useState } from "react";
import type { PhoneView } from "../api";
import { ago } from "../format";

export function Badge({
	tone = "neutral",
	children,
	title,
}: {
	tone?: "neutral" | "good" | "warn" | "bad" | "info";
	children: ReactNode;
	title?: string;
}) {
	const tones = {
		neutral: "bg-zinc-100 text-zinc-700 dark:bg-zinc-800 dark:text-zinc-300",
		good: "bg-emerald-100 text-emerald-800 dark:bg-emerald-950 dark:text-emerald-300",
		warn: "bg-amber-100 text-amber-900 dark:bg-amber-950 dark:text-amber-300",
		bad: "bg-red-100 text-red-800 dark:bg-red-950 dark:text-red-300",
		info: "bg-sky-100 text-sky-800 dark:bg-sky-950 dark:text-sky-300",
	};
	return (
		<span
			title={title}
			className={`inline-flex items-center gap-1 rounded-md px-1.5 py-0.5 text-xs font-medium whitespace-nowrap ${tones[tone]}`}
		>
			{children}
		</span>
	);
}

// Four bars, filled up to Android's 0-4 signal level.
export function SignalBars({
	level,
	title,
}: {
	level: number;
	title?: string;
}) {
	return (
		<span
			className="inline-flex items-end gap-0.5"
			title={title}
			role="img"
			aria-label={`signal ${level} of 4`}
		>
			{[1, 2, 3, 4].map((bar) => (
				<span
					key={bar}
					className={`w-1 rounded-sm ${bar <= level ? "bg-zinc-800 dark:bg-zinc-200" : "bg-zinc-300 dark:bg-zinc-700"}`}
					style={{ height: `${bar * 3 + 2}px` }}
				/>
			))}
		</span>
	);
}

// The note is page text, so the browser's find-in-page sees it; the pencil
// opens a text box. Enter (without Shift) or the check saves, Escape cancels.
export function NoteEditor({
	value,
	placeholder = "Add a note",
	onSave,
	onEditingChange,
}: {
	value: string;
	placeholder?: string;
	onSave: (value: string) => Promise<void>;
	onEditingChange?: (editing: boolean) => void;
}) {
	const [editing, setEditingState] = useState(false);
	const setEditing = (next: boolean) => {
		setEditingState(next);
		onEditingChange?.(next);
	};
	const [draft, setDraft] = useState(value);
	const [error, setError] = useState<string | null>(null);
	useEffect(() => {
		if (!editing) setDraft(value);
	}, [value, editing]);
	if (!editing)
		return value ? (
			<div className="flex items-start gap-1.5 text-sm">
				<span className="break-words whitespace-pre-wrap">{value}</span>
				<button
					type="button"
					onClick={() => setEditing(true)}
					className="mt-0.5 shrink-0 rounded p-0.5 text-zinc-400 hover:bg-zinc-100 hover:text-zinc-900 dark:hover:bg-zinc-800 dark:hover:text-zinc-100"
					aria-label="Edit note"
					title="Edit note"
				>
					<Pencil size={12} />
				</button>
			</div>
		) : (
			<button
				type="button"
				onClick={() => setEditing(true)}
				className="inline-flex items-center gap-1 text-left text-sm text-zinc-400 italic hover:text-zinc-700 dark:text-zinc-500 dark:hover:text-zinc-300"
			>
				<Pencil size={12} />
				{placeholder}
			</button>
		);
	const save = async () => {
		try {
			await onSave(draft.trim());
			setEditing(false);
			setError(null);
		} catch (reason) {
			setError(String(reason instanceof Error ? reason.message : reason));
		}
	};
	return (
		<div className="flex flex-col gap-1">
			<div className="flex items-start gap-1">
				<textarea
					// biome-ignore lint/a11y/noAutofocus: the editor opens because the user clicked to edit.
					autoFocus
					rows={2}
					value={draft}
					onChange={(event) => setDraft(event.target.value)}
					onKeyDown={(event) => {
						if (event.key === "Enter" && !event.shiftKey) {
							event.preventDefault();
							void save();
						}
						if (event.key === "Escape") setEditing(false);
					}}
					className="w-full rounded-md border bg-transparent px-2 py-1 text-sm outline-none focus:border-sky-500"
				/>
				<button
					type="button"
					onClick={() => void save()}
					className="p-1 text-emerald-600"
					aria-label="Save note"
				>
					<Check size={16} />
				</button>
				<button
					type="button"
					onClick={() => setEditing(false)}
					className="p-1 text-zinc-500"
					aria-label="Cancel"
				>
					<X size={16} />
				</button>
			</div>
			{error && <span className="text-xs text-red-600">{error}</span>}
		</div>
	);
}

export function Empty({
	icon,
	title,
	children,
}: {
	icon: ReactNode;
	title: string;
	children?: ReactNode;
}) {
	return (
		<div className="flex flex-col items-center gap-2 rounded-xl border border-dashed px-6 py-16 text-center">
			<span className="text-zinc-400">{icon}</span>
			<h2 className="font-medium">{title}</h2>
			{children && (
				<div className="max-w-md text-sm text-zinc-500 dark:text-zinc-400">
					{children}
				</div>
			)}
		</div>
	);
}

// A phone's adb state in words.
export function StatusBadge({ phone }: { phone: PhoneView }) {
	switch (phone.status) {
		case "online":
			return <Badge tone="good">Online</Badge>;
		case "connecting":
			return <Badge tone="info">Connecting</Badge>;
		case "needs_auth":
			return <Badge tone="warn">Needs approval</Badge>;
		case "no_permissions":
			return <Badge tone="bad">No USB permission</Badge>;
		case "other":
			return <Badge tone="warn">{phone.adbState}</Badge>;
		default:
			return (
				<Badge tone="bad" title={phone.lastOnlineAt ?? undefined}>
					Offline
					{phone.lastOnlineAt ? ` · seen ${ago(phone.lastOnlineAt)}` : ""}
				</Badge>
			);
	}
}
