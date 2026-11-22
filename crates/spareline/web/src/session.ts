import { useCallback, useEffect, useState } from "react";
import { ApiError, api, SIGNED_OUT_EVENT } from "./api";

export type Session =
	| { state: "loading" }
	| { state: "signed-out"; noUsers: boolean }
	| { state: "signed-in"; name: string };

// Who is signed in. Any API call that answers 401 signs the page out, such
// as after the session expired or the password changed elsewhere.
export function useSession() {
	const [session, setSession] = useState<Session>({ state: "loading" });

	const refresh = useCallback(async () => {
		const response = await fetch("/api/v1/auth/me");
		const body = (await response.json()) as {
			user?: { name: string };
			noUsers?: boolean;
		};
		setSession(
			response.ok && body.user
				? { state: "signed-in", name: body.user.name }
				: { state: "signed-out", noUsers: body.noUsers ?? false },
		);
	}, []);

	useEffect(() => {
		void refresh();
		const signedOut = () => void refresh();
		window.addEventListener(SIGNED_OUT_EVENT, signedOut);
		return () => window.removeEventListener(SIGNED_OUT_EVENT, signedOut);
	}, [refresh]);

	const signIn = useCallback(async (name: string, password: string) => {
		const result = await api<{ user: { name: string } }>("/auth/login", {
			method: "POST",
			body: { name, password },
		});
		setSession({ state: "signed-in", name: result.user.name });
	}, []);

	const signOut = useCallback(async () => {
		try {
			await api("/auth/logout", { method: "POST", body: {} });
		} catch (error) {
			if (!(error instanceof ApiError && error.status === 401)) throw error;
		}
		setSession({ state: "signed-out", noUsers: false });
	}, []);

	return { session, signIn, signOut };
}
