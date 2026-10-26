// Failed sign-ins per key (a user name, a client address), in memory. A key
// with `max` failures inside `windowMs` is refused until its oldest one is
// that old. A restart forgets them, which costs an attacker a restart they
// cannot cause.
export class LoginLimiter {
	private readonly failures = new Map<string, number[]>();

	constructor(
		private readonly max = 10,
		private readonly windowMs = 15 * 60_000,
		private readonly now: () => number = () => Date.now(),
	) {}

	// Seconds to wait when any key is over the limit, otherwise null.
	retryAfter(keys: string[]): number | null {
		let wait = 0;
		for (const key of keys) {
			const recent = this.recent(key);
			if (recent.length >= this.max)
				wait = Math.max(
					wait,
					(recent[0] as number) + this.windowMs - this.now(),
				);
		}
		return wait > 0 ? Math.ceil(wait / 1000) : null;
	}

	fail(keys: string[]): void {
		for (const key of keys)
			this.failures.set(key, [...this.recent(key), this.now()]);
	}

	// A correct password clears the name's failures; the address keeps its
	// own, so one good account does not unlock guessing at others.
	succeed(keys: string[]): void {
		for (const key of keys)
			if (key.startsWith("name:")) this.failures.delete(key);
	}

	private recent(key: string): number[] {
		const cutoff = this.now() - this.windowMs;
		const recent = (this.failures.get(key) ?? []).filter((at) => at > cutoff);
		if (recent.length === 0) this.failures.delete(key);
		else this.failures.set(key, recent);
		return recent;
	}
}
