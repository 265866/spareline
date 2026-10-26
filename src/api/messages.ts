import { existsSync } from "node:fs";
import { Elysia } from "elysia";
import { z } from "zod";
import type { Bus } from "../bus";
import { toE164 } from "../devices/identifiers";
import { type Inventory, NotFoundError } from "../inventory/store";
import { attachmentPath } from "../sms/attachments";
import type { Message, MessageFilter, MessageStore } from "../sms/store";
import { BadRequestError } from "./errors";
import {
	MessagePage,
	MessagesQuery,
	NumberMessagesQuery,
} from "./message-schema";
import { ApiError } from "./schema";

export type MessageOptions = {
	store: MessageStore;
	bus: Bus<Message>;
	attachmentsDir: string;
};

type PageQuery = {
	limit?: number;
	before?: number;
	after?: number;
	wait?: number;
};

const DEFAULT_LIMIT = 50;

// A number by E.164 (+15125550101), by digits (15125550101 or 5125550101,
// read as US), or by its id (up to 9 digits, which no phone number has), so
// a number whose SIM does not report it can still be addressed.
export function findNumberId(inventory: Inventory, key: string): number | null {
	const trimmed = key.trim();
	if (/^\d{1,9}$/.test(trimmed)) return inventory.numberIdOf(trimmed);
	const e164 = trimmed.startsWith("+")
		? toE164(trimmed, null)
		: (toE164(trimmed, "us") ??
			(/^\d{10,15}$/.test(trimmed) ? `+${trimmed}` : null));
	return e164 ? inventory.numberIdOf(e164) : null;
}

// One page of messages: newest first by default (optionally before a seq),
// or oldest first after a seq, waiting up to `wait` seconds for the first
// new one. A waiting request wakes on every ingested message and re-reads
// with its own filter, so it returns exactly what a fresh request would.
export async function messagePage(
	options: MessageOptions,
	filter: Omit<MessageFilter, "limit" | "order" | "after" | "before">,
	query: PageQuery,
	signal: AbortSignal,
): Promise<{ messages: Message[]; cursor: number }> {
	if (query.before !== undefined && query.after !== undefined)
		throw new BadRequestError("use before or after, not both");
	const limit = query.limit ?? DEFAULT_LIMIT;
	const { store } = options;
	if (query.after === undefined) {
		// Read the cursor first: a message committed in between is then newer
		// than the cursor and not lost by a client that polls from it.
		const cursor = store.maxSeq();
		const messages = store.list({
			...filter,
			before: query.before,
			limit,
			order: "desc",
		});
		return { messages, cursor };
	}
	const after = query.after;
	const read = () => store.list({ ...filter, after, limit, order: "asc" });
	const messages = await waitFor(options.bus, read, query.wait ?? 0, signal);
	return { messages, cursor: messages.at(-1)?.seq ?? after };
}

function waitFor(
	bus: Bus<Message>,
	read: () => Message[],
	waitSeconds: number,
	signal: AbortSignal,
): Promise<Message[]> {
	const first = read();
	if (first.length > 0 || waitSeconds <= 0 || signal.aborted)
		return Promise.resolve(first);
	return new Promise((resolve) => {
		const finish = (messages: Message[]) => {
			clearTimeout(timer);
			unsubscribe();
			signal.removeEventListener("abort", onAbort);
			resolve(messages);
		};
		const onAbort = () => finish([]);
		const timer = setTimeout(() => finish([]), waitSeconds * 1000);
		const unsubscribe = bus.subscribe(() => {
			const messages = read();
			if (messages.length > 0) finish(messages);
		});
		signal.addEventListener("abort", onAbort, { once: true });
	});
}

const pageDescription =
	"Without `after`: newest first, optionally `before` a seq. With `after`: oldest first, and `wait` (0-30 s) holds the request open until a message arrives, answering `{messages: [], cursor: after}` on timeout. A text stored before its number was known reappears with a higher seq once it is; (phone, provider, providerId, receivedAt) identifies a text across seqs.";

export function messageRoutes(inventory: Inventory, options: MessageOptions) {
	return new Elysia({ prefix: "/api/v1" })
		.get(
			"/numbers/:key/messages",
			({ params, query, request, server }) => {
				const id = findNumberId(inventory, params.key);
				if (id === null) throw new NotFoundError(`no number ${params.key}`);
				// Bun closes idle connections after 10 s by default.
				if (query.wait) server?.timeout(request, query.wait + 10);
				return messagePage(options, { numberId: id }, query, request.signal);
			},
			{
				params: z.object({
					key: z.string().describe("E.164, digits, or number id"),
				}),
				query: NumberMessagesQuery,
				response: { 200: MessagePage, 400: ApiError, 404: ApiError },
				detail: {
					summary: "Texts to one number",
					description: pageDescription,
					tags: ["messages"],
				},
			},
		)
		.get(
			"/messages",
			({ query, request, server }) => {
				if (query.wait) server?.timeout(request, query.wait + 10);
				return messagePage(
					options,
					{ search: query.q?.trim() || undefined },
					query,
					request.signal,
				);
			},
			{
				query: MessagesQuery,
				response: { 200: MessagePage, 400: ApiError },
				detail: {
					summary: "Texts to every number",
					description: pageDescription,
					tags: ["messages"],
				},
			},
		)
		.get(
			"/attachments/:id",
			({ params }) => {
				const attachment = options.store.attachment(params.id);
				const path =
					attachment &&
					attachmentPath(options.attachmentsDir, attachment.sha256);
				if (!attachment || !path || !existsSync(path))
					throw new NotFoundError(`no attachment ${params.id}`);
				// Inline only for raster images; anything else (HTML, SVG) downloads
				// and is sandboxed, so a hostile part cannot run script here.
				const inline = /^image\/(png|jpeg|gif|webp|bmp|heic|heif)$/.test(
					attachment.contentType,
				);
				const name = (attachment.filename ?? `attachment-${params.id}`).replace(
					/[^\w.-]/g,
					"_",
				);
				return new Response(Bun.file(path), {
					headers: {
						"content-type": attachment.contentType,
						"content-disposition": `${inline ? "inline" : "attachment"}; filename="${name}"`,
						"cache-control": "private, max-age=31536000, immutable",
						"x-content-type-options": "nosniff",
						"content-security-policy": "sandbox; default-src 'none'",
					},
				});
			},
			{
				params: z.object({ id: z.coerce.number().int().min(1) }),
				detail: {
					summary: "The bytes of an MMS attachment, with its content type",
					tags: ["messages"],
				},
			},
		);
}
