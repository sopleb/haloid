/**
 * Copy text. `navigator.clipboard` needs a secure context, and self-hosters
 * on plain HTTP over a LAN are common, so fall back to the old
 * `execCommand` route through a hidden textarea.
 */
export async function copy(text: string): Promise<boolean> {
	if (globalThis.isSecureContext && navigator.clipboard) {
		try {
			await navigator.clipboard.writeText(text);
			return true;
		} catch {
			// Denied: try the old way.
		}
	}
	const area = document.createElement('textarea');
	area.value = text;
	area.setAttribute('readonly', '');
	area.style.position = 'fixed';
	area.style.opacity = '0';
	document.body.append(area);
	area.select();
	try {
		return document.execCommand('copy');
	} finally {
		area.remove();
	}
}
