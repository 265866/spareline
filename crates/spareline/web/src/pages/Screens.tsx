import { AndroidMotionEventAction } from "@yume-chan/scrcpy";
import {
	ArrowLeft,
	ChevronLeft,
	Circle,
	MonitorPlay,
	MonitorSmartphone,
	RotateCw,
	Square,
} from "lucide-react";
import {
	type PointerEvent as ReactPointerEvent,
	type RefObject,
	useEffect,
	useRef,
	useState,
} from "react";
import { type PhoneView, useResource } from "../api";
import { Badge, Empty, StatusBadge } from "../components/ui";
import { formatNumber } from "../format";
import { linkProps } from "../router";
import { ScreenStream, type StreamState } from "../screen-stream";

// Android key codes for the navigation buttons and the keys that are not
// typed as text.
const KEY = { back: 4, home: 3, recents: 187 } as const;
const SPECIAL_KEYS: Record<string, number> = {
	Enter: 66,
	NumpadEnter: 66,
	Backspace: 67,
	Delete: 112,
	Tab: 61,
	Escape: 111,
	ArrowUp: 19,
	ArrowDown: 20,
	ArrowLeft: 21,
	ArrowRight: 22,
	Home: 122,
	End: 123,
	PageUp: 92,
	PageDown: 93,
};

function phoneModel(phone: PhoneView | undefined): string {
	return (
		[phone?.manufacturer, phone?.model].filter(Boolean).join(" ") || "Phone"
	);
}

// The numbers of the phone's SIMs.
function phoneNumbers(phone: PhoneView | undefined): string {
	const numbers = (phone?.slots ?? [])
		.map((slot) => slot.number?.e164)
		.filter((e164): e164 is string => !!e164)
		.map((e164) => formatNumber(e164));
	return numbers.join(", ") || "No number";
}

// The phone's stream on the canvas. A browser that cannot play the video
// gets a stream in the "unsupported" state that says so.
function useStream(serial: string): {
	canvas: RefObject<HTMLCanvasElement | null>;
	stream: RefObject<ScreenStream | null>;
	state: StreamState | null;
} {
	const canvas = useRef<HTMLCanvasElement>(null);
	const stream = useRef<ScreenStream | null>(null);
	const [state, setState] = useState<StreamState | null>(null);
	useEffect(() => {
		if (!canvas.current) return;
		const current = new ScreenStream(canvas.current, serial, setState);
		stream.current = current;
		return () => {
			current.dispose();
			stream.current = null;
			setState(null);
		};
	}, [serial]);
	return { canvas, stream, state };
}

function StreamBadge({
	phone,
	state,
}: {
	phone: PhoneView | undefined;
	state: StreamState | null;
}) {
	if (phone && phone.status !== "online")
		return <Badge tone="bad">Offline</Badge>;
	switch (state?.phase) {
		case "live":
			return <Badge tone="good">Live</Badge>;
		case "starting":
		case "connecting":
			return <Badge tone="info">Starting</Badge>;
		case "restarting":
			return <Badge tone="warn">Restarting</Badge>;
		case "offline":
			return <Badge tone="bad">Offline</Badge>;
		case "failed":
			return (
				<Badge tone="bad" title={state.message ?? undefined}>
					{state.stopped ? "Failed" : "Failed, retrying"}
				</Badge>
			);
		case "unsupported":
			return <Badge tone="bad">Can't play video</Badge>;
		default:
			return <Badge>Paused</Badge>;
	}
}

// Every phone as a static tile. Nothing streams until a tile is opened.
export function ScreensPage() {
	const phones = useResource<{ phones: PhoneView[] }>("/phones");
	const list = phones.data?.phones;
	const online = list?.filter((phone) => phone.status === "online") ?? [];
	const others = list?.filter((phone) => phone.status !== "online") ?? [];
	return (
		<div className="flex flex-col gap-4">
			<div>
				<h1 className="text-xl font-semibold">Screens</h1>
				<p className="text-sm text-zinc-500 dark:text-zinc-400">
					{list
						? `${online.length} of ${list.length} phones online. Open a phone to see and control its screen.`
						: "Loading…"}
				</p>
			</div>
			{phones.error && (
				<Badge tone="bad">Could not load phones: {phones.error}</Badge>
			)}
			{list?.length === 0 && (
				<Empty icon={<MonitorSmartphone size={32} />} title="No phones yet">
					Plug a phone into the host with USB debugging on.
				</Empty>
			)}
			<div className="grid grid-cols-[repeat(auto-fill,minmax(220px,1fr))] gap-4">
				{[...online, ...others].map((phone) => (
					<ScreenTile key={phone.serial} phone={phone} />
				))}
			</div>
		</div>
	);
}

function RetryButton({ onClick }: { onClick: () => void }) {
	return (
		<button
			type="button"
			onClick={onClick}
			className="inline-flex items-center gap-1.5 rounded-md border bg-white px-2.5 py-1.5 text-sm text-zinc-900 hover:bg-zinc-100 dark:bg-zinc-900 dark:text-zinc-100 dark:hover:bg-zinc-800"
		>
			<RotateCw size={14} />
			Retry
		</button>
	);
}

function ScreenTile({ phone }: { phone: PhoneView }) {
	return (
		<a
			{...linkProps(`/screens/${encodeURIComponent(phone.serial)}`)}
			className="flex flex-col gap-3 rounded-xl border bg-white p-3 shadow-sm hover:border-zinc-400 dark:bg-zinc-900 dark:hover:border-zinc-600"
		>
			<div className="flex items-start justify-between gap-2">
				<div className="min-w-0">
					<div className="truncate font-medium">
						{phoneModel(phone)}{" "}
						<span
							className="font-mono text-xs font-normal text-zinc-500"
							title={phone.serial}
						>
							…{phone.serial.slice(-4)}
						</span>
					</div>
					<div className="truncate text-sm text-zinc-500 dark:text-zinc-400">
						{phoneNumbers(phone)}
					</div>
				</div>
				<StatusBadge phone={phone} />
			</div>
			{phone.status === "online" && (
				<span className="inline-flex items-center gap-1.5 text-sm text-sky-700 dark:text-sky-400">
					<MonitorPlay size={15} />
					View screen
				</span>
			)}
		</a>
	);
}

export function ScreenFocusPage({ serial }: { serial: string }) {
	const phones = useResource<{ phones: PhoneView[] }>("/phones");
	const phone = phones.data?.phones.find((p) => p.serial === serial);
	const { canvas, stream, state } = useStream(serial);
	const surface = useRef<HTMLDivElement>(null);
	const pressed = useRef(new Set<number>());

	const toVideo = (event: { clientX: number; clientY: number }) => {
		const rect = canvas.current?.getBoundingClientRect();
		if (!rect || !state?.width || !state.height || rect.width === 0)
			return null;
		return {
			x: ((event.clientX - rect.left) / rect.width) * state.width,
			y: ((event.clientY - rect.top) / rect.height) * state.height,
		};
	};
	const pointer = (
		event: ReactPointerEvent<HTMLCanvasElement>,
		action: AndroidMotionEventAction,
	) => {
		const point = toVideo(event);
		if (!point) return;
		const id =
			event.pointerType === "touch" ? BigInt(event.pointerId) : undefined;
		stream.current?.touch(action, point.x, point.y, id);
	};

	// React's wheel listener is passive, so it cannot stop the page scrolling.
	useEffect(() => {
		const element = canvas.current;
		if (!element) return;
		const onWheel = (event: WheelEvent) => {
			event.preventDefault();
			const point = toVideo(event);
			if (point)
				stream.current?.scroll(point.x, point.y, event.deltaX, event.deltaY);
		};
		element.addEventListener("wheel", onWheel, { passive: false });
		return () => element.removeEventListener("wheel", onWheel);
	});

	const onKeyDown = (event: React.KeyboardEvent) => {
		const current = stream.current;
		if (!current || event.metaKey || (event.ctrlKey && event.key === "v"))
			return;
		const code = SPECIAL_KEYS[event.code];
		if (code !== undefined) current.key(code);
		else if (event.key.length === 1 && !event.ctrlKey && !event.altKey)
			current.text(event.key);
		else return;
		event.preventDefault();
	};

	const button = (label: string, icon: React.ReactNode, action: () => void) => (
		<button
			type="button"
			onClick={() => {
				action();
				surface.current?.focus();
			}}
			disabled={state?.phase !== "live"}
			title={label}
			className="inline-flex items-center gap-1.5 rounded-md border bg-white px-2.5 py-1.5 text-sm hover:bg-zinc-100 disabled:opacity-50 dark:bg-zinc-900 dark:hover:bg-zinc-800"
		>
			{icon}
			{label}
		</button>
	);

	return (
		<div className="flex flex-col gap-3">
			<div className="flex flex-wrap items-center gap-x-4 gap-y-2">
				<a
					{...linkProps("/screens")}
					className="inline-flex items-center gap-1 text-sm text-zinc-600 hover:text-zinc-900 dark:text-zinc-400 dark:hover:text-zinc-100"
				>
					<ArrowLeft size={15} />
					All screens
				</a>
				<h1 className="text-lg font-semibold">{phoneModel(phone)}</h1>
				<span className="font-mono text-xs text-zinc-500">{serial}</span>
				<span className="text-sm text-zinc-600 dark:text-zinc-300">
					{phoneNumbers(phone)}
				</span>
				<StreamBadge phone={phone} state={state} />
				{state?.phase === "live" && (
					<span className="text-xs text-zinc-500 tabular-nums dark:text-zinc-400">
						{state.width}×{state.height} · {state.fps.toFixed(0)} fps ·{" "}
						{state.kbps} kbps
					</span>
				)}
			</div>
			<div className="flex flex-wrap gap-2">
				{button("Back", <ChevronLeft size={15} />, () =>
					stream.current?.key(KEY.back),
				)}
				{button("Home", <Circle size={13} />, () =>
					stream.current?.key(KEY.home),
				)}
				{button("Recents", <Square size={13} />, () =>
					stream.current?.key(KEY.recents),
				)}
			</div>
			<div
				ref={surface}
				role="application"
				aria-label="Phone screen"
				// biome-ignore lint/a11y/noNoninteractiveTabindex: the remote screen must take focus to receive typed keys.
				tabIndex={0}
				onKeyDown={onKeyDown}
				onPaste={(event) => {
					const text = event.clipboardData.getData("text");
					if (text) stream.current?.text(text);
					event.preventDefault();
				}}
				className="relative flex w-fit justify-center self-center rounded-xl bg-zinc-950 p-2 outline-none focus-visible:ring-2 focus-visible:ring-sky-500"
			>
				<canvas
					ref={canvas}
					onPointerDown={(event) => {
						surface.current?.focus();
						event.currentTarget.setPointerCapture(event.pointerId);
						pressed.current.add(event.pointerId);
						pointer(event, AndroidMotionEventAction.Down);
					}}
					onPointerMove={(event) => {
						if (pressed.current.has(event.pointerId))
							pointer(event, AndroidMotionEventAction.Move);
					}}
					onPointerUp={(event) => {
						if (!pressed.current.delete(event.pointerId)) return;
						pointer(event, AndroidMotionEventAction.Up);
					}}
					onPointerCancel={(event) => {
						if (!pressed.current.delete(event.pointerId)) return;
						pointer(event, AndroidMotionEventAction.Up);
					}}
					onContextMenu={(event) => event.preventDefault()}
					className="block max-h-[calc(100vh-14rem)] max-w-full touch-none rounded-lg"
					style={
						state?.width && state.height
							? { aspectRatio: `${state.width} / ${state.height}` }
							: { width: 270, height: 600 }
					}
				/>
				{state?.phase !== "live" && (
					<div className="absolute inset-0 flex flex-col items-center justify-center gap-3 p-6 text-center text-sm text-zinc-400">
						{state?.message ?? "Connecting…"}
						{state?.stopped && (
							<RetryButton onClick={() => stream.current?.retry()} />
						)}
					</div>
				)}
			</div>
			<p className="text-center text-xs text-zinc-500 dark:text-zinc-400">
				Click or drag on the screen to touch, scroll to swipe. Click the screen
				first to type; paste works.
			</p>
		</div>
	);
}
