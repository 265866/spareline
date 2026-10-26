export type Log = (event: string, fields?: Record<string, unknown>) => void;

// One JSON object per line on stderr, which the container log keeps.
export const log: Log = (event, fields = {}) =>
	console.error(
		JSON.stringify({ at: new Date().toISOString(), event, ...fields }),
	);

export const silent: Log = () => {};
