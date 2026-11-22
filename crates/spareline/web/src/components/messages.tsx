import { Paperclip } from "lucide-react";
import type { AttachmentView, MessageView } from "../api";
import { ago, formatNumber } from "../format";
import { linkProps } from "../router";

function exact(iso: string): string {
	return new Date(iso).toLocaleString(undefined, {
		dateStyle: "medium",
		timeStyle: "medium",
	});
}

// Senders are leads' numbers, which stay visible; short codes and names are
// shown as the phone reports them.
function sender(from: string | null): string {
	if (!from) return "Unknown sender";
	return /^\+\d{8,15}$/.test(from) ? formatNumber(from) : from;
}

function size(bytes: number): string {
	if (bytes < 1024) return `${bytes} B`;
	if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} KB`;
	return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

function Attachment({ attachment }: { attachment: AttachmentView }) {
	if (/^image\/(png|jpeg|gif|webp|bmp)$/.test(attachment.contentType))
		return (
			<a href={attachment.url} target="_blank" rel="noreferrer">
				<img
					src={attachment.url}
					alt={attachment.filename ?? "Picture"}
					loading="lazy"
					className="max-h-64 max-w-full rounded-lg border object-contain"
				/>
			</a>
		);
	return (
		<a
			href={attachment.url}
			className="inline-flex items-center gap-1 rounded-md border px-2 py-1 text-xs text-sky-700 hover:bg-zinc-50 dark:text-sky-400 dark:hover:bg-zinc-800"
		>
			<Paperclip size={12} />
			{attachment.filename ?? attachment.contentType} · {size(attachment.size)}
		</a>
	);
}

// The carrier holds texts while a phone is out of service; a long gap
// between sending and receiving is worth showing.
function Held({ message }: { message: MessageView }) {
	if (!message.sentAt) return null;
	const held = Date.parse(message.receivedAt) - Date.parse(message.sentAt);
	if (held < 10 * 60_000) return null;
	return (
		<span
			className="text-xs text-amber-700 dark:text-amber-400"
			title={`Sent ${exact(message.sentAt)}`}
		>
			sent{" "}
			{ago(message.sentAt, Date.parse(message.receivedAt)).replace(" ago", "")}{" "}
			before it arrived
		</span>
	);
}

export function MessageList({
	messages,
	fresh,
	showNumber,
	now,
}: {
	messages: MessageView[];
	fresh: ReadonlySet<number>;
	showNumber: boolean;
	now: number;
}) {
	return (
		<ol className="divide-y overflow-hidden rounded-xl border bg-white shadow-sm dark:bg-zinc-900">
			{messages.map((message) => (
				<li
					key={message.seq}
					className={`flex flex-col gap-1.5 px-4 py-3 sm:flex-row sm:gap-4 ${
						fresh.has(message.seq) ? "bg-sky-50 dark:bg-sky-950/40" : ""
					}`}
				>
					<div className="flex shrink-0 flex-wrap items-baseline gap-x-3 gap-y-0.5 text-sm sm:w-56 sm:flex-col">
						<time
							dateTime={message.receivedAt}
							title={`Received ${exact(message.receivedAt)} (phone clock)\nStored ${exact(message.ingestedAt)}`}
							className="text-xs text-zinc-500 tabular-nums dark:text-zinc-400"
						>
							{ago(message.receivedAt, now)}
						</time>
						<span className="font-medium tabular-nums">
							{sender(message.from)}
						</span>
						{showNumber && (
							<span className="text-xs text-zinc-500 tabular-nums dark:text-zinc-400">
								to{" "}
								{message.number ? (
									<a
										{...linkProps(`/numbers/${message.number.id}`)}
										className="hover:underline"
									>
										{message.number.e164
											? formatNumber(message.number.e164)
											: "number not detected"}
									</a>
								) : (
									<span title="The SIM that received it is not known yet">
										unassigned SIM
									</span>
								)}
								{message.number?.note && (
									<span className="block break-words whitespace-pre-wrap">
										{message.number.note}
									</span>
								)}
							</span>
						)}
					</div>
					<div className="flex min-w-0 flex-1 flex-col gap-2">
						{message.subject && (
							<span className="text-sm font-medium">{message.subject}</span>
						)}
						{message.body && (
							<p className="text-sm break-words whitespace-pre-wrap">
								{message.body}
							</p>
						)}
						{message.attachments.length > 0 && (
							<div className="flex flex-wrap gap-2">
								{message.attachments.map((attachment) => (
									<Attachment key={attachment.id} attachment={attachment} />
								))}
							</div>
						)}
						<Held message={message} />
					</div>
				</li>
			))}
		</ol>
	);
}
