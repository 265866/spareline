import { useEffect, useState } from "react";

// A tiny pathname router: the server answers every non-API path with the SPA.
function navigate(path: string): void {
	if (path === location.pathname + location.search) return;
	history.pushState(null, "", path);
	window.dispatchEvent(new PopStateEvent("popstate"));
}

export function useLocation(): URL {
	const [url, setUrl] = useState(() => new URL(location.href));
	useEffect(() => {
		const update = () => setUrl(new URL(location.href));
		window.addEventListener("popstate", update);
		return () => window.removeEventListener("popstate", update);
	}, []);
	return url;
}

// Sets or clears one query parameter without adding a history entry.
export function setQuery(name: string, value: string | null): void {
	const url = new URL(location.href);
	if (value === null) url.searchParams.delete(name);
	else url.searchParams.set(name, value);
	history.replaceState(null, "", url);
	window.dispatchEvent(new PopStateEvent("popstate"));
}

export function linkProps(path: string) {
	return {
		href: path,
		onClick: (event: {
			preventDefault: () => void;
			metaKey: boolean;
			ctrlKey: boolean;
			button: number;
		}) => {
			if (event.metaKey || event.ctrlKey || event.button !== 0) return;
			event.preventDefault();
			navigate(path);
		},
	};
}
