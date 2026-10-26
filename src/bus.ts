// An in-process publish/subscribe channel. A subscriber that throws does not
// stop delivery to the others.
export class Bus<T> {
	private readonly subscribers = new Set<(value: T) => void>();

	publish(value: T): void {
		for (const subscriber of this.subscribers) {
			try {
				subscriber(value);
			} catch (error) {
				console.error(
					JSON.stringify({
						at: new Date().toISOString(),
						event: "bus-subscriber-failed",
						error: String(error),
					}),
				);
			}
		}
	}

	subscribe(subscriber: (value: T) => void): () => void {
		this.subscribers.add(subscriber);
		return () => this.subscribers.delete(subscriber);
	}
}
