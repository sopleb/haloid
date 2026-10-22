import type { ConfigView } from '$lib/api/types';

/** Why a file cannot be uploaded, before a byte leaves — or `null`. */
export function preflight(file: Pick<File, 'name' | 'size'>, config: ConfigView): string | null {
	const uploads = config.uploads;
	if (!uploads.allowed) return 'Uploads are not open to you here.';
	if (file.size === 0) return 'That file is empty.';
	if (file.size > uploads.max_size) return `That file is ${size(file.size)}; the limit is ${size(uploads.max_size)}.`;
	const extension = file.name.includes('.') ? file.name.split('.').pop()!.toLowerCase() : '';
	// The server decides the real type from the bytes; this only catches
	// the obvious, so nobody waits for a doomed upload.
	if (extension && !uploads.extensions.includes(extension)) {
		return `.${extension} files are not accepted.`;
	}
	return null;
}

/** Bytes as people read them, in powers of 1024. */
export function size(bytes: number): string {
	const units = ['B', 'KB', 'MB', 'GB', 'TB'];
	let value = bytes;
	let unit = 0;
	while (value >= 1024 && unit < units.length - 1) {
		value /= 1024;
		unit += 1;
	}
	// One decimal under ten, so 1.5 MB is not rounded to 2; `1.0` reads as 1.
	const shown = unit === 0 || value >= 10 ? Math.round(value) : Math.round(value * 10) / 10;
	return `${shown} ${units[unit]}`;
}
