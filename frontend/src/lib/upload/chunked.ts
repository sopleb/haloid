// Sending a big file in chunks, one after another, resumable. The server's
// record of what it holds is the only one trusted: resuming always asks it
// first and sends only what is missing.
import { ApiError, type UploadOptions } from '$lib/api/client';
import type { OpenSessionBody, SessionView, UploadResponse } from '$lib/api/types';

/** Files above this go up in chunks; below it, one request is simpler. */
export const CHUNKED_ABOVE = 16 * 1024 * 1024;

export interface SessionApi {
	open(body: OpenSessionBody): Promise<SessionView>;
	state(id: string): Promise<SessionView>;
	put(
		id: string,
		n: number,
		chunk: Blob,
		options: Pick<UploadOptions, 'onProgress' | 'signal'>
	): Promise<SessionView>;
	complete(id: string, signal?: AbortSignal): Promise<UploadResponse>;
}

export interface ChunkedOptions {
	signal: AbortSignal;
	onProgress: (fraction: number) => void;
	fields?: Record<string, string>;
	/** A session to resume, from an earlier attempt. */
	session?: string | null;
	/** Told the session id as soon as there is one, to resume with later. */
	onSession?: (id: string) => void;
}

/** The upload form's fields, as the session body wants them. */
export function sessionBody(file: Pick<File, 'name' | 'size'>, fields: Record<string, string> = {}): OpenSessionBody {
	const tags = fields.tags
		?.split(',')
		.map((t) => t.trim())
		.filter(Boolean);
	return {
		filename: file.name || 'upload',
		size: file.size,
		...(fields.title ? { title: fields.title } : {}),
		...(fields.description ? { description: fields.description } : {}),
		...(tags?.length ? { tags } : {}),
		...(fields.album ? { album: fields.album } : {}),
		...(fields.nsfw ? { nsfw: fields.nsfw === 'true' || fields.nsfw === '1' } : {}),
		...(fields.expires_in ? { expires_in: Number(fields.expires_in) } : {})
	};
}

function aborted(signal: AbortSignal) {
	if (signal.aborted) throw new DOMException('Upload cancelled', 'AbortError');
}

export async function uploadInChunks(
	file: File,
	api: SessionApi,
	options: ChunkedOptions
): Promise<UploadResponse> {
	let session: SessionView | null = null;
	if (options.session) {
		try {
			session = await api.state(options.session);
		} catch (e) {
			// Expired or never ours: start again rather than fail.
			if (!(e instanceof ApiError && e.problem.status === 404)) throw e;
		}
		if (session && session.status !== 'open') session = null;
	}
	aborted(options.signal);
	if (!session) {
		session = await api.open(sessionBody(file, options.fields));
		options.onSession?.(session.id);
	}

	const size = file.size;
	const lengthOf = (n: number) => Math.min(session!.chunk_size, size - n * session!.chunk_size);
	let held = session.received.reduce((sum, n) => sum + lengthOf(n), 0);
	options.onProgress(size ? held / size : 0);

	for (const n of session.missing) {
		aborted(options.signal);
		const start = n * session.chunk_size;
		const chunk = file.slice(start, start + lengthOf(n));
		await api.put(session.id, n, chunk, {
			signal: options.signal,
			onProgress: (fraction) => options.onProgress((held + fraction * chunk.size) / size)
		});
		held += chunk.size;
		options.onProgress(held / size);
	}
	aborted(options.signal);
	return api.complete(session.id, options.signal);
}
