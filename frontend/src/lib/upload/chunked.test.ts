import { describe, expect, it } from 'vitest';
import { ApiError } from '$lib/api/client';
import type { SessionView, UploadResponse } from '$lib/api/types';
import { sessionBody, uploadInChunks, type SessionApi } from './chunked';

/** A fake server holding chunks in memory. */
function server(chunkSize: number) {
	const sessions = new Map<string, { size: number; received: Set<number>; bytes: Map<number, number> }>();
	const puts: number[] = [];
	let next = 1;
	const view = (id: string): SessionView => {
		const s = sessions.get(id)!;
		const total = Math.ceil(s.size / chunkSize);
		const all = [...Array(total).keys()];
		return {
			id,
			chunk_size: chunkSize,
			chunks_total: total,
			received: all.filter((n) => s.received.has(n)),
			missing: all.filter((n) => !s.received.has(n)),
			status: 'open',
			expires_at: ''
		};
	};
	let failAt: number | null = null;
	const api: SessionApi = {
		open: async (body) => {
			const id = `s${next++}`;
			sessions.set(id, { size: body.size, received: new Set(), bytes: new Map() });
			return view(id);
		},
		state: async (id) => {
			if (!sessions.has(id)) {
				throw new ApiError({ type: 'about:blank', title: 'x', status: 404, detail: 'gone', code: 'not_found' });
			}
			return view(id);
		},
		put: async (id, n, chunk, options) => {
			if (failAt === n) {
				failAt = null;
				throw new ApiError({ type: 'about:blank', title: 'x', status: 0, detail: 'offline', code: 'network' });
			}
			options.onProgress?.(0.5);
			puts.push(n);
			sessions.get(id)!.received.add(n);
			sessions.get(id)!.bytes.set(n, chunk.size);
			return view(id);
		},
		complete: async (id) => ({ file: { id } as UploadResponse['file'], duplicate: false }) as UploadResponse
	};
	return { api, puts, sessions, failNext: (n: number) => (failAt = n) };
}

const file = (bytes: number) => new File([new Uint8Array(bytes)], 'big.mp4', { type: 'video/mp4' });

describe('chunked uploads', () => {
	it('sends every chunk in order, the last one short, and completes', async () => {
		const { api, puts, sessions } = server(10);
		const progress: number[] = [];
		let session = '';
		const done = await uploadInChunks(file(25), api, {
			signal: new AbortController().signal,
			onProgress: (f) => progress.push(f),
			onSession: (id) => (session = id)
		});
		expect(done.file.id).toBe(session);
		expect(puts).toEqual([0, 1, 2]);
		expect([...sessions.get(session)!.bytes.values()]).toEqual([10, 10, 5]);
		expect(progress.at(-1)).toBe(1);
		expect(progress).toEqual([...progress].sort((a, b) => a - b));
	});

	it('resumes by asking the server, sending only what is missing', async () => {
		const { api, puts, failNext } = server(10);
		let session = '';
		failNext(2);
		await expect(
			uploadInChunks(file(40), api, {
				signal: new AbortController().signal,
				onProgress: () => {},
				onSession: (id) => (session = id)
			})
		).rejects.toThrow('offline');
		expect(puts).toEqual([0, 1]);

		const progress: number[] = [];
		await uploadInChunks(file(40), api, {
			signal: new AbortController().signal,
			onProgress: (f) => progress.push(f),
			session
		});
		expect(puts).toEqual([0, 1, 2, 3]);
		expect(progress[0]).toBe(0.5);
	});

	it('starts afresh when the session to resume has gone', async () => {
		const { api, puts } = server(10);
		let session = '';
		await uploadInChunks(file(15), api, {
			signal: new AbortController().signal,
			onProgress: () => {},
			session: 'expired',
			onSession: (id) => (session = id)
		});
		expect(session).toBe('s1');
		expect(puts).toEqual([0, 1]);
	});

	it('stops at once when cancelled', async () => {
		const { api, puts } = server(10);
		const controller = new AbortController();
		controller.abort();
		await expect(
			uploadInChunks(file(30), api, { signal: controller.signal, onProgress: () => {} })
		).rejects.toThrow('cancelled');
		expect(puts).toEqual([]);
	});

	it('carries the form fields into the session', () => {
		expect(
			sessionBody({ name: 'a.mp4', size: 9 }, { title: 'T', tags: 'x, y,', nsfw: 'true', album: 'abc' })
		).toEqual({ filename: 'a.mp4', size: 9, title: 'T', tags: ['x', 'y'], nsfw: true, album: 'abc' });
	});
});
