import { Hash, MessageSquare, Smartphone } from "lucide-react";
import type { ReactNode } from "react";
import { type StatusView, useDeviceEvents, useResource } from "./api";
import { Badge, Empty } from "./components/ui";
import { ago, EVENT_LABEL } from "./format";
import { MessagesPage } from "./pages/Messages";
import { NumberPage, NumbersPage } from "./pages/Numbers";
import { PhonesPage } from "./pages/Phones";
import { linkProps, useLocation } from "./router";

const NAV = [
	{ path: "/", label: "Phones", icon: Smartphone },
	{ path: "/numbers", label: "Numbers", icon: Hash },
	{ path: "/messages", label: "Messages", icon: MessageSquare },
] as const;

export function App() {
	const url = useLocation();
	const path = url.pathname;
	const { connected, recent } = useDeviceEvents();
	const status = useResource<StatusView>("/status", 15_000);
	const adb = status.data?.adb;
	const numberMatch = /^\/numbers\/([^/]+)$/.exec(path);

	let page: ReactNode;
	if (path === "/" || path === "/phones") page = <PhonesPage />;
	else if (numberMatch?.[1])
		page = <NumberPage id={decodeURIComponent(numberMatch[1])} />;
	else if (path === "/numbers")
		page = <NumbersPage search={url.searchParams.get("q")} />;
	else if (path === "/messages")
		page = <MessagesPage search={url.searchParams.get("q")} />;
	else page = <Empty icon={<Smartphone size={32} />} title="Page not found" />;

	return (
		<div className="min-h-screen">
			<header className="sticky top-0 z-10 border-b bg-white/90 backdrop-blur dark:bg-zinc-950/90">
				<div className="mx-auto flex max-w-screen-2xl flex-wrap items-center gap-x-6 gap-y-2 px-4 py-2.5">
					<span className="font-semibold">spareline</span>
					<nav className="-mx-1 flex max-w-full gap-1 overflow-x-auto px-1">
						{NAV.map((item) => {
							const active =
								item.path === "/"
									? path === "/" || path === "/phones"
									: path.startsWith(item.path);
							const Icon = item.icon;
							return (
								<a
									key={item.path}
									{...linkProps(item.path)}
									className={`inline-flex items-center gap-1.5 rounded-md px-2.5 py-1.5 text-sm ${
										active
											? "bg-zinc-900 text-white dark:bg-zinc-100 dark:text-zinc-900"
											: "text-zinc-600 hover:bg-zinc-100 dark:text-zinc-300 dark:hover:bg-zinc-900"
									}`}
								>
									<Icon size={15} />
									{item.label}
								</a>
							);
						})}
					</nav>
					<div className="ml-auto flex items-center gap-2 text-xs">
						{adb && (
							<Badge
								tone={adb.reachable ? "good" : "bad"}
								title={adb.error ?? `adb protocol ${adb.version}`}
							>
								{adb.reachable
									? "adb connected"
									: `adb unreachable since ${ago(adb.since)}`}
							</Badge>
						)}
						{!connected && <Badge tone="warn">Live updates paused</Badge>}
					</div>
				</div>
			</header>
			<main className="mx-auto flex max-w-screen-2xl flex-col gap-8 px-4 py-6">
				{page}
				{(path === "/" || path === "/phones") && recent.length > 0 && (
					<section className="flex flex-col gap-2">
						<h2 className="text-sm font-medium text-zinc-500 dark:text-zinc-400">
							Recent activity
						</h2>
						<ul className="divide-y rounded-xl border bg-white text-sm dark:bg-zinc-900">
							{recent.slice(0, 8).map((event) => (
								<li
									key={event.id}
									className="flex flex-wrap items-center gap-x-3 gap-y-1 px-4 py-2"
								>
									<span
										className="w-20 shrink-0 text-xs text-zinc-500"
										title={event.at}
									>
										{ago(event.at)}
									</span>
									<span className="font-medium">
										{EVENT_LABEL[event.kind] ?? event.kind}
									</span>
									{event.serial && (
										<span className="font-mono text-xs text-zinc-500">
											{event.serial}
										</span>
									)}
								</li>
							))}
						</ul>
					</section>
				)}
			</main>
		</div>
	);
}
