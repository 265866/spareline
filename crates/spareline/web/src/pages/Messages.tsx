import { MessageSquare, Search } from "lucide-react";
import { useEffect, useState } from "react";
import { useMessages, useNow } from "../api";
import { MessageList } from "../components/messages";
import { Badge, Empty } from "../components/ui";
import { setQuery } from "../router";

// Every incoming text across all numbers, newest first, live.
export function MessagesPage({ search }: { search: string | null }) {
	const { messages, loaded, error, hasOlder, loadOlder, fresh } = useMessages(
		`/messages${search ? `?q=${encodeURIComponent(search)}` : ""}`,
	);
	const now = useNow();
	return (
		<div className="flex flex-col gap-4">
			<div className="flex flex-wrap items-end justify-between gap-3">
				<div>
					<h1 className="text-xl font-semibold">All messages</h1>
					<p className="text-sm text-zinc-500 dark:text-zinc-400">
						Every incoming text, newest first. New ones appear as they arrive.
					</p>
				</div>
				<SearchBox value={search ?? ""} />
			</div>
			{error && <Badge tone="bad">Could not load messages: {error}</Badge>}
			{loaded && messages.length === 0 && (
				<Empty
					icon={<MessageSquare size={32} />}
					title={search ? "No texts match" : "No texts yet"}
				>
					{search
						? "Try another search."
						: "Texts appear here within seconds of reaching a phone."}
				</Empty>
			)}
			{messages.length > 0 && (
				<MessageList messages={messages} fresh={fresh} showNumber now={now} />
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
		</div>
	);
}

// Searches as you type, after a short pause.
function SearchBox({ value }: { value: string }) {
	const [draft, setDraft] = useState(value);
	useEffect(() => setDraft(value), [value]);
	useEffect(() => {
		if (draft === value) return;
		const timer = setTimeout(
			() => setQuery("q", draft.trim() === "" ? null : draft.trim()),
			300,
		);
		return () => clearTimeout(timer);
	}, [draft, value]);
	return (
		<label className="flex items-center gap-1.5 rounded-md border bg-white px-2 py-1.5 text-sm focus-within:border-sky-500 dark:bg-zinc-900">
			<Search size={14} className="text-zinc-400" />
			<input
				type="search"
				value={draft}
				onChange={(event) => setDraft(event.target.value)}
				placeholder="Text, sender, number, or note"
				aria-label="Search messages"
				className="w-56 bg-transparent outline-none"
			/>
		</label>
	);
}
