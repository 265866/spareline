import { ArrowLeft, Hash, MessageSquare, Search } from "lucide-react";
import { useMemo, useState } from "react";
import {
	ApiError,
	api,
	type NumberView,
	useMessages,
	useNow,
	useResource,
} from "../api";
import { MessageList } from "../components/messages";
import { Badge, Empty, NoteEditor, SignalBars } from "../components/ui";
import { ago, formatNumber, SERVICE_LABEL } from "../format";
import { linkProps, setQuery } from "../router";
import { visibleNumbers } from "../search";

function Where({ number }: { number: NumberView }) {
	if (!number.phone)
		return <span className="text-zinc-400">No SIM linked</span>;
	const phone = `${number.phone.model ?? "Phone"} · SIM ${number.phone.slot + 1}`;
	const serial = (
		<span className="block font-mono text-xs text-zinc-500 dark:text-zinc-400">
			{number.phone.serial}
		</span>
	);
	if (number.phone.removedAt)
		return (
			<span className="text-amber-700 dark:text-amber-400">
				SIM removed {ago(number.phone.removedAt)} (was in {phone}){serial}
			</span>
		);
	return (
		<span>
			{phone}
			{serial}
		</span>
	);
}

function Service({ number }: { number: NumberView }) {
	if (number.phoneStatus === null) return null;
	if (number.phoneStatus !== "online")
		return <Badge tone="bad">Phone offline</Badge>;
	if (!number.radio) return <Badge>Unknown</Badge>;
	return (
		<span className="inline-flex items-center gap-2">
			<Badge tone={number.radio.voice === "in_service" ? "good" : "warn"}>
				{SERVICE_LABEL[number.radio.voice] ?? number.radio.voice}
			</Badge>
			<SignalBars level={number.radio.signalLevel} />
		</span>
	);
}

// Every number is rendered as page text (no paging), so the browser's
// find-in-page searches all of them; the search box narrows the rows.
export function NumbersPage({ search }: { search: string | null }) {
	const numbers = useResource<{ numbers: NumberView[] }>("/numbers");
	const list = numbers.data?.numbers;
	const query = search ?? "";
	const [editing, setEditing] = useState<ReadonlySet<number>>(new Set());
	const shown = useMemo(
		() => list && visibleNumbers(list, query, editing),
		[list, query, editing],
	);
	const trackEditing = (id: number, open: boolean) =>
		setEditing((current) => {
			const next = new Set(current);
			if (open) next.add(id);
			else next.delete(id);
			return next;
		});
	const saveNote = async (id: number, note: string) => {
		await api(`/numbers/${id}`, { method: "PATCH", body: { note } });
		numbers.reload();
	};
	return (
		<div className="flex flex-col gap-4">
			<div className="flex flex-wrap items-end justify-between gap-3">
				<div>
					<h1 className="text-xl font-semibold">Numbers</h1>
					<p className="text-sm text-zinc-500 dark:text-zinc-400">
						{!list || !shown
							? "Loading…"
							: shown.length === list.length
								? `${list.length} number${list.length === 1 ? "" : "s"}`
								: `${shown.length} of ${list.length} numbers`}
					</p>
				</div>
				<label className="flex items-center gap-1.5 rounded-md border bg-white px-2 py-1.5 text-sm focus-within:border-sky-500 dark:bg-zinc-900">
					<Search size={14} className="text-zinc-400" />
					<input
						type="search"
						value={query}
						onChange={(event) => setQuery("q", event.target.value || null)}
						placeholder="Number in any format, note, or phone"
						title="Matches any part of a number however it is written, such as (512) 555 or +1512, its note, or its phone and SIM slot"
						aria-label="Search numbers"
						className="w-80 bg-transparent outline-none"
					/>
				</label>
			</div>
			{numbers.error && (
				<Badge tone="bad">Could not load numbers: {numbers.error}</Badge>
			)}
			{list?.length === 0 && (
				<Empty icon={<Hash size={32} />} title="No numbers yet">
					A number appears when a phone with a SIM comes online.
				</Empty>
			)}
			{list && list.length > 0 && shown?.length === 0 && (
				<Empty icon={<Search size={32} />} title="No numbers match">
					Search by any part of a number in any format, its note, or its phone.
				</Empty>
			)}
			{shown && shown.length > 0 && (
				<div className="overflow-x-auto rounded-xl border bg-white shadow-sm dark:bg-zinc-900">
					<table className="w-full text-left text-sm">
						<thead className="border-b text-xs text-zinc-500 uppercase dark:text-zinc-400">
							<tr>
								<th className="px-4 py-2 font-medium">Number</th>
								<th className="px-4 py-2 font-medium">Note</th>
								<th className="px-4 py-2 font-medium">Where</th>
								<th className="px-4 py-2 font-medium">Service</th>
							</tr>
						</thead>
						<tbody className="divide-y">
							{shown.map((number) => (
								<tr
									key={number.id}
									className="align-top hover:bg-zinc-50 dark:hover:bg-zinc-800/50"
								>
									<td className="px-4 py-2.5 whitespace-nowrap tabular-nums">
										<a
											{...linkProps(`/numbers/${number.id}`)}
											className="font-medium hover:underline"
										>
											{number.e164 ? formatNumber(number.e164) : "Not detected"}
										</a>
										{number.source === "manual" && (
											<span className="ml-1 text-xs text-zinc-500">
												(set by hand)
											</span>
										)}
										{number.e164 && (
											<span className="block font-mono text-xs text-zinc-500 dark:text-zinc-400">
												{number.e164}
											</span>
										)}
									</td>
									<td className="max-w-xs min-w-48 px-4 py-2.5 text-zinc-700 dark:text-zinc-200">
										<NoteEditor
											value={number.note}
											onSave={(note) => saveNote(number.id, note)}
											onEditingChange={(open) => trackEditing(number.id, open)}
										/>
									</td>
									<td className="px-4 py-2.5 whitespace-nowrap">
										<Where number={number} />
									</td>
									<td className="px-4 py-2.5 whitespace-nowrap">
										<Service number={number} />
									</td>
								</tr>
							))}
						</tbody>
					</table>
				</div>
			)}
		</div>
	);
}

export function NumberPage({ id }: { id: string }) {
	const number = useResource<NumberView>(`/numbers/${encodeURIComponent(id)}`);
	const save = async (patch: { note?: string; e164?: string | null }) => {
		await api(`/numbers/${encodeURIComponent(id)}`, {
			method: "PATCH",
			body: patch,
		});
		number.reload();
	};
	const value = number.data;
	return (
		<div className="flex flex-col gap-4">
			<a
				{...linkProps("/numbers")}
				className="inline-flex w-fit items-center gap-1 text-sm text-zinc-500 hover:underline"
			>
				<ArrowLeft size={14} /> Numbers
			</a>
			{number.error && <Badge tone="bad">{number.error}</Badge>}
			{value && (
				<>
					<div className="flex flex-col gap-3 rounded-xl border bg-white p-5 shadow-sm dark:bg-zinc-900">
						<div className="flex flex-wrap items-center justify-between gap-3">
							<h1 className="text-2xl font-semibold tabular-nums">
								{value.e164 ? formatNumber(value.e164) : "Number not detected"}
							</h1>
							<Service number={value} />
						</div>
						<div className="text-sm text-zinc-600 dark:text-zinc-300">
							<Where number={value} />
							{value.sim && (
								<span className="ml-2 font-mono text-xs text-zinc-500">
									ICCID {value.sim.iccid}
								</span>
							)}
						</div>
						<NumberOverride number={value} onSave={(e164) => save({ e164 })} />
						<NoteEditor
							value={value.note}
							placeholder="Add a note, such as where this number is advertised"
							onSave={(note) => save({ note })}
						/>
					</div>
					<NumberMessages id={id} />
				</>
			)}
		</div>
	);
}

// Every text to this number, newest first, live.
function NumberMessages({ id }: { id: string }) {
	const { messages, loaded, error, hasOlder, loadOlder, fresh } = useMessages(
		`/numbers/${encodeURIComponent(id)}/messages`,
	);
	const now = useNow();
	return (
		<section className="flex flex-col gap-3">
			<h2 className="text-sm font-medium text-zinc-500 dark:text-zinc-400">
				Texts{loaded ? ` (${messages.length}${hasOlder ? "+" : ""})` : ""}
			</h2>
			{error && <Badge tone="bad">Could not load texts: {error}</Badge>}
			{loaded && messages.length === 0 && (
				<Empty icon={<MessageSquare size={32} />} title="No texts yet">
					Texts to this number appear here within seconds of reaching the phone.
				</Empty>
			)}
			{messages.length > 0 && (
				<MessageList
					messages={messages}
					fresh={fresh}
					showNumber={false}
					now={now}
				/>
			)}
			{hasOlder && (
				<button
					type="button"
					onClick={loadOlder}
					className="self-center rounded-md border px-3 py-1.5 text-sm hover:bg-zinc-100 dark:hover:bg-zinc-900"
				>
					Show older texts
				</button>
			)}
		</section>
	);
}

// The detected number is used unless the owner types a correction.
function NumberOverride({
	number,
	onSave,
}: {
	number: NumberView;
	onSave: (e164: string | null) => Promise<void>;
}) {
	const [editing, setEditing] = useState(false);
	const [draft, setDraft] = useState("");
	const [error, setError] = useState<string | null>(null);
	const submit = async (value: string | null) => {
		try {
			await onSave(value);
			setEditing(false);
			setError(null);
		} catch (reason) {
			setError(reason instanceof ApiError ? reason.message : String(reason));
		}
	};
	const detected = number.detectedE164
		? formatNumber(number.detectedE164)
		: "nothing";
	return (
		<div className="flex flex-col gap-1 text-sm">
			<div className="flex flex-wrap items-center gap-2 text-zinc-500 dark:text-zinc-400">
				{number.source === "manual" ? (
					<>
						<span>
							Set by hand{number.confirmedBy ? ` by ${number.confirmedBy}` : ""}
							; the SIM reports {detected}.
						</span>
						<button
							type="button"
							className="text-sky-600 hover:underline"
							onClick={() => void submit(null)}
						>
							Use the detected number
						</button>
					</>
				) : (
					<>
						<span>
							{number.source === "detected"
								? "Detected from the SIM."
								: "The SIM does not report its number."}
						</span>
						{!editing && (
							<button
								type="button"
								className="text-sky-600 hover:underline"
								onClick={() => setEditing(true)}
							>
								{number.source === "none" ? "Enter the number" : "Correct it"}
							</button>
						)}
					</>
				)}
			</div>
			{editing && (
				<form
					className="flex items-center gap-2"
					onSubmit={(event) => {
						event.preventDefault();
						void submit(draft);
					}}
				>
					<input
						// biome-ignore lint/a11y/noAutofocus: the field opens because the user asked to edit.
						autoFocus
						value={draft}
						onChange={(event) => setDraft(event.target.value)}
						placeholder="+1 512 555 0101"
						className="w-48 rounded-md border bg-transparent px-2 py-1 outline-none focus:border-sky-500"
					/>
					<button
						type="submit"
						className="rounded-md bg-zinc-900 px-3 py-1 text-white dark:bg-zinc-100 dark:text-zinc-900"
					>
						Save
					</button>
					<button
						type="button"
						className="text-zinc-500"
						onClick={() => setEditing(false)}
					>
						Cancel
					</button>
				</form>
			)}
			{error && <span className="text-xs text-red-600">{error}</span>}
		</div>
	);
}
