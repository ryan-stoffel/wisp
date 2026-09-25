/*---------------------------------------------------------------------------------------------
 *  wisp: not part of Code - OSS. Edit editor/overlay in the wisp repo, not this copy.
 *--------------------------------------------------------------------------------------------*/

/**
 * A version 7 UUID (RFC 9562): a 48-bit Unix timestamp in milliseconds, then random bits. The
 * protocol's client-generated ids use it (decision record 0007), so they sort by creation time.
 */
export function generateUuidV7(now: number = Date.now()): string {
	const bytes = new Uint8Array(16);
	crypto.getRandomValues(bytes);

	let time = Math.floor(now);
	for (let i = 5; i >= 0; i--) {
		bytes[i] = time % 256;
		time = Math.floor(time / 256);
	}
	bytes[6] = 0x70 | (bytes[6] & 0x0f);
	bytes[8] = 0x80 | (bytes[8] & 0x3f);

	let hex = '';
	for (const byte of bytes) {
		hex += byte.toString(16).padStart(2, '0');
	}
	return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}
