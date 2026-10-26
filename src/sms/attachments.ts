import { createHash } from "node:crypto";
import {
	closeSync,
	existsSync,
	fsyncSync,
	mkdirSync,
	openSync,
	renameSync,
	writeSync,
} from "node:fs";
import { join } from "node:path";

// MMS parts are files named by their sha256, so the same image sent twice is
// stored once and a crash mid-write leaves no half file under the final name.
export function saveAttachment(
	dir: string,
	bytes: Uint8Array,
): { sha256: string; size: number } {
	const sha256 = createHash("sha256").update(bytes).digest("hex");
	const path = attachmentPath(dir, sha256);
	if (!existsSync(path)) {
		mkdirSync(dir, { recursive: true });
		const temp = `${path}.${process.pid}.${Date.now()}.tmp`;
		// Synced before the rename, so a power cut cannot leave the final name
		// pointing at data that never reached the card.
		const fd = openSync(temp, "w");
		try {
			for (let offset = 0; offset < bytes.length; )
				offset += writeSync(fd, bytes, offset);
			fsyncSync(fd);
		} finally {
			closeSync(fd);
		}
		renameSync(temp, path);
	}
	return { sha256, size: bytes.length };
}

export function attachmentPath(dir: string, sha256: string): string {
	if (!/^[0-9a-f]{64}$/.test(sha256)) throw new Error("not a sha256");
	return join(dir, sha256);
}
