import { useCallback, useEffect, useRef, useState } from "react";
import type {
	AttachmentView,
	MessagePageView,
	MessageView,
} from "../src/api/message-schema";
import type {
	DeviceEventView,
	NumberView,
	PhoneView,
	StatusView,
} from "../src/api/schema";

export type {
	AttachmentView,
	DeviceEventView,
	MessagePageView,
	MessageView,
	NumberView,
	PhoneView,
	StatusView,
};

// Window events re-dispatched from the one EventSource (useDeviceEvents).
export const MESSAGE_EVENT = "message-event";
export const STREAM_OPEN_EVENT = "stream-open";
// A request was refused for want of a session (see useSession).
export const SIGNED_OUT_EVENT = "signed-out";

export class ApiError extends Error {
	constructor(
		readonly status: number,
		message: string,
	) {
		super(message);
	}
}

export async function api<T>(
	path: string,
	init: { method?: string; body?: unknown } = {},
): Promise<T> {
	const response = await fetch(`/api/v1${path}`, {
		method: init.method ?? "GET",
		headers:
			init.body === undefined ? {} : { "content-type": "application/json" },
		body: init.body === undefined ? undefined : JSON.stringify(init.body),
	});
	if (!response.ok) {
		if (response.status === 401 && path !== "/auth/login")
			window.dispatchEvent(new Event(SIGNED_OUT_EVENT));
		const text = await response.text();
		let message = text;
		try {
			message = (JSON.parse(text) as { error?: string }).error ?? text;
		} catch {
			// Not JSON; keep the text.
		}
		throw new ApiError(response.status, message || response.statusText);
	}
	return response.status === 204
		? (undefined as T)
		: ((await response.json()) as T);
}

type Resource<T> = {
	data: T | undefined;
	error: string | null;
	reload: () => void;
};

// Fetches `path`, refetches every `intervalMs`, and whenever a device event
// or a text arrives (see useDeviceEvents).
export function useResource<T>(path: string, intervalMs = 15_000): Resource<T> {
	const [data, setData] = useState<T>();
	const [error, setError] = useState<string | null>(null);
	const loadNow = useRef(() => {});
	const reload = useCallback(() => loadNow.current(), []);
	useEffect(() => {
		let cancelled = false;
		const load = () =>
			api<T>(path)
				.then((value) => {
					if (cancelled) return;
					setData(value);
					setError(null);
				})
				.catch((reason: unknown) => {
					if (!cancelled)
						setError(String(reason instanceof Error ? reason.message : reason));
				});
		loadNow.current = () => void load();
		void load();
		const timer = setInterval(load, intervalMs);
		const onEvent = () => void load();
		window.addEventListener("device-event", onEvent);
		return () => {
			cancelled = true;
			clearInterval(timer);
			window.removeEventListener("device-event", onEvent);
		};
	}, [path, intervalMs]);
	return { data, error, reload };
}

// One EventSource for the whole page, carrying device events and texts. A
// burst of either makes every useResource refetch once ("device-event"); each
// text is also re-dispatched as MESSAGE_EVENT, and STREAM_OPEN_EVENT tells
// message lists to catch up after a reconnect.
export function useDeviceEvents(): {
	connected: boolean;
	recent: DeviceEventView[];
} {
	const [connected, setConnected] = useState(false);
	const [recent, setRecent] = useState<DeviceEventView[]>([]);
	useEffect(() => {
		// The stream starts with the newest 20 device events and new texts; on
		// reconnect the browser sends Last-Event-ID ("<deviceEventId>:<seq>")
		// and the server resumes both after it.
		const source = new EventSource(
			"/api/v1/events?types=device,message&latest=20",
		);
		let refetch: ReturnType<typeof setTimeout> | undefined;
		// One refetch for a burst of events, such as the replay.
		const refetchSoon = () => {
			clearTimeout(refetch);
			refetch = setTimeout(
				() => window.dispatchEvent(new Event("device-event")),
				100,
			);
		};
		source.onopen = () => {
			setConnected(true);
			window.dispatchEvent(new Event(STREAM_OPEN_EVENT));
		};
		source.onerror = () => setConnected(false);
		source.addEventListener("device", (message) => {
			const event = JSON.parse(
				(message as MessageEvent<string>).data,
			) as DeviceEventView;
			setRecent((events) => [event, ...events].slice(0, 20));
			refetchSoon();
		});
		source.addEventListener("message", (message) => {
			const text = JSON.parse(
				(message as MessageEvent<string>).data,
			) as MessageView;
			window.dispatchEvent(
				new CustomEvent<MessageView>(MESSAGE_EVENT, { detail: text }),
			);
			refetchSoon();
		});
		return () => {
			clearTimeout(refetch);
			source.close();
		};
	}, []);
	return { connected, recent };
}

// A live list of texts for `path` (a messages endpoint with its filters):
// the newest page first, then anything after the cursor whenever a text
// arrives or the stream reconnects, and older pages on request. The server
// applies the filters, so a text for another number simply adds nothing.
export function useMessages(path: string, pageSize = 50) {
	const [messages, setMessages] = useState<MessageView[]>([]);
	const [loaded, setLoaded] = useState(false);
	const [error, setError] = useState<string | null>(null);
	const [hasOlder, setHasOlder] = useState(false);
	// Seqs that arrived after the first page, for a brief highlight.
	const [fresh, setFresh] = useState<ReadonlySet<number>>(new Set());
	const list = useRef<MessageView[]>([]);
	const loadOlderNow = useRef(() => {});
	useEffect(() => {
		const join = path.includes("?") ? "&" : "?";
		let cancelled = false;
		let cursor: number | null = null;
		let busy = false;
		let again = false;
		const show = (next: MessageView[]) => {
			list.current = next;
			setMessages(next);
		};
		// A text stored before its number was known comes back at a higher seq
		// once it is; the newer copy replaces the older one.
		const identity = (m: MessageView) =>
			`${m.phone}|${m.provider}|${m.providerId}|${m.receivedAt}`;
		const merge = (incoming: MessageView[], at: "top" | "bottom") => {
			const known = new Set(list.current.map((m) => m.seq));
			const added = incoming.filter((m) => !known.has(m.seq));
			const replaced = new Set(added.map(identity));
			const rest =
				at === "top"
					? list.current.filter((m) => !replaced.has(identity(m)))
					: list.current;
			show(at === "top" ? [...added, ...rest] : [...rest, ...added]);
			return added;
		};
		const fail = (reason: unknown) => {
			if (!cancelled)
				setError(String(reason instanceof Error ? reason.message : reason));
		};
		const catchUp = async (): Promise<void> => {
			if (cursor === null) return;
			if (busy) {
				again = true;
				return;
			}
			busy = true;
			try {
				do {
					again = false;
					const page: MessagePageView = await api<MessagePageView>(
						`${path}${join}after=${cursor}&limit=500`,
					);
					if (cancelled) return;
					cursor = page.cursor;
					const added = merge(page.messages.slice().reverse(), "top");
					if (added.length > 0)
						setFresh((set) => new Set([...set, ...added.map((m) => m.seq)]));
					if (page.messages.length === 500) again = true;
				} while (again);
				setError(null);
			} catch (reason) {
				fail(reason);
			} finally {
				busy = false;
			}
		};
		show([]);
		setLoaded(false);
		setError(null);
		setFresh(new Set());
		void api<MessagePageView>(`${path}${join}limit=${pageSize}`)
			.then((page) => {
				if (cancelled) return;
				cursor = page.cursor;
				show(page.messages);
				setHasOlder(page.messages.length === pageSize);
				setLoaded(true);
				return catchUp();
			})
			.catch(fail);
		loadOlderNow.current = () => {
			const oldest = list.current.at(-1)?.seq;
			if (oldest === undefined) return;
			void api<MessagePageView>(
				`${path}${join}before=${oldest}&limit=${pageSize}`,
			)
				.then((page) => {
					if (cancelled) return;
					setHasOlder(page.messages.length === pageSize);
					merge(page.messages, "bottom");
				})
				.catch(fail);
		};
		const onChange = () => void catchUp();
		window.addEventListener(MESSAGE_EVENT, onChange);
		window.addEventListener(STREAM_OPEN_EVENT, onChange);
		return () => {
			cancelled = true;
			window.removeEventListener(MESSAGE_EVENT, onChange);
			window.removeEventListener(STREAM_OPEN_EVENT, onChange);
		};
	}, [path, pageSize]);
	const loadOlder = useCallback(() => loadOlderNow.current(), []);
	return { messages, loaded, error, hasOlder, loadOlder, fresh };
}

// Re-renders every `intervalMs` so relative times stay current.
export function useNow(intervalMs = 30_000): number {
	const [now, setNow] = useState(() => Date.now());
	useEffect(() => {
		const timer = setInterval(() => setNow(Date.now()), intervalMs);
		return () => clearInterval(timer);
	}, [intervalMs]);
	return now;
}
