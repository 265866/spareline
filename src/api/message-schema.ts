import { z } from "zod";

// The HTTP contract for texts. The web UI imports only the inferred types.

const iso = z.string().describe("ISO 8601 UTC time");

export const Attachment = z.object({
	id: z.number().int(),
	contentType: z.string(),
	size: z.number().int().describe("Bytes"),
	filename: z.string().nullable(),
	url: z.string().describe("GET this path for the bytes"),
});

const exampleMessage = {
	seq: 11,
	provider: "sms" as const,
	providerId: 11,
	phone: "SPARELINE00001",
	subId: 1,
	simId: 1,
	number: { id: 1, e164: "+15125550101", note: "Facebook ad, blue couch" },
	from: "+15125550199",
	body: "Is the couch still available?",
	subject: null,
	sentAt: "2026-10-21T19:50:13.000Z",
	receivedAt: "2026-10-21T19:50:27.282Z",
	ingestedAt: "2026-10-21T19:50:29.904Z",
	attachments: [],
};

export const Message = z
	.object({
		seq: z
			.number()
			.int()
			.describe("Global, increasing cursor across all numbers and phones"),
		provider: z.enum(["sms", "mms"]),
		providerId: z.number().int().describe("_id in the phone's provider"),
		phone: z.string().describe("Serial of the phone that received it"),
		subId: z.number().int().nullable(),
		simId: z.number().int().nullable(),
		number: z
			.object({
				id: z.number().int(),
				e164: z.string().nullable(),
				note: z.string(),
			})
			.nullable()
			.describe(
				"The number it was sent to; null while the SIM is not known yet (unassigned SIM)",
			),
		from: z.string().nullable().describe("Sender as the phone reports it"),
		body: z.string(),
		subject: z.string().nullable().describe("MMS subject"),
		sentAt: iso.nullable().describe("Carrier time (date_sent)"),
		receivedAt: iso.describe("Phone clock (date)"),
		ingestedAt: iso.describe("Server clock"),
		attachments: z.array(Attachment),
	})
	.meta({ examples: [exampleMessage] });

export const MessagePage = z
	.object({
		messages: z.array(Message),
		cursor: z
			.number()
			.int()
			.describe(
				"Pass as `after` to get only newer messages. Newest-first pages return the highest seq stored when they were read; `after` pages return the last seq returned, or `after` itself when nothing arrived.",
			),
	})
	.meta({ examples: [{ messages: [exampleMessage], cursor: 11 }] });

const limit = z.coerce.number().int().min(1).max(500).optional();
const seq = z.coerce.number().int().min(0).optional();

export const NumberMessagesQuery = z.object({
	limit: limit.describe("Default 50"),
	before: seq.describe("Newest first, older than this seq"),
	after: seq.describe("Oldest first, newer than this seq"),
	wait: z.coerce
		.number()
		.int()
		.min(0)
		.max(30)
		.optional()
		.describe(
			"With `after`: seconds to wait for a new message before answering with an empty list",
		),
});

export const MessagesQuery = NumberMessagesQuery.extend({
	q: z
		.string()
		.max(200)
		.optional()
		.describe(
			'Substring of the body, subject, sender, number, or the number\'s note, ignoring case; a query of digits and phone punctuation, such as "(512) 555", also matches the number and sender by digits',
		),
});

export type MessageView = z.infer<typeof Message>;
export type MessagePageView = z.infer<typeof MessagePage>;
export type AttachmentView = z.infer<typeof Attachment>;
