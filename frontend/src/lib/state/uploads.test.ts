import { describe, expect, it } from 'vitest';
import { ApiError } from '$lib/api/client';
import type { UploadResponse } from '$lib/api/types';
import type { Pending, PendingStore } from '$lib/upload/resume';
import { UploadQueue, type Transport } from './uploads.svelte';

function memoryStore() {
	const kept = new Map<string, Pending>();
	const store: PendingStore = {
		save: async (p) => void kept.set(p.session, p),
		remove: async (session) => void kept.delete(session),
		all: async () => [...kept.values()]
	};
	return { store, kept };
}

/** A transport whose uploads finish when the test says so. */
function fakeTransport() {
	const calls: {
		file: File;
		fields?: Record<string, string>;
		progress: (f: number) => void;
		resolve: (r: UploadResponse) => void;
		reject: (e: unknown) => void;
		signal: AbortSignal;
		session: string | null;
		onSession: (id: string) => void;
	}[] = [];
	const transport: Transport = (file, options) =>
		new Promise((resolve, reject) => {
			options.signal.addEventListener('abort', () =>
				reject(new DOMException('Upload cancelled', 'AbortError'))
			);
			calls.push({
				file,
				fields: options.fields,
				progress: options.onProgress,
				resolve,
				reject,
				signal: options.signal,
				session: options.session,
				onSession: options.onSession
			});
		});
	return { transport, calls };
}

const file = (name: string, type = 'image/png') => new File(['x'], name, { type });
const response = (id: string): UploadResponse =>
	({ file: { id } as UploadResponse['file'], duplicate: false }) as UploadResponse;
const settle = () => new Promise((r) => setTimeout(r, 0));

function queue(overrides: ConstructorParameters<typeof UploadQueue>[1] = {}) {
	const fake = fakeTransport();
	const revoked: string[] = [];
	const flushes: (() => void)[] = [];
	let n = 0;
	const q = new UploadQueue(fake.transport, {
		concurrency: 2,
		createPreview: (f) => (f.type.startsWith('image/') ? `blob:${++n}` : null),
		revokePreview: (url) => revoked.push(url),
		schedule: (flush) => flushes.push(flush),
		...overrides
	});
	return { q, ...fake, revoked, flushes };
}

describe('the upload queue', () => {
	it('keeps no more than `concurrency` in flight', async () => {
		const { q, calls } = queue();
		q.add([file('a.png'), file('b.png'), file('c.png')]);
		expect(calls.map((c) => c.file.name)).toEqual(['a.png', 'b.png']);
		expect(q.items.map((i) => i.status)).toEqual(['uploading', 'uploading', 'queued']);

		calls[0].resolve(response('A'));
		await settle();
		expect(calls.map((c) => c.file.name)).toEqual(['a.png', 'b.png', 'c.png']);
		expect(q.items[0].status).toBe('done');
		expect(q.items[0].result?.id).toBe('A');
	});

	it('flushes progress once per frame, not once per event', () => {
		const { q, calls, flushes } = queue();
		q.add([file('a.png')]);
		calls[0].progress(0.1);
		calls[0].progress(0.4);
		calls[0].progress(0.7);
		expect(flushes).toHaveLength(1);
		expect(q.items[0].progress).toBe(0);
		flushes[0]();
		expect(q.items[0].progress).toBe(0.7);
	});

	it('cancelling aborts the request, and a queued file never starts', async () => {
		const { q, calls } = queue({ concurrency: 1 });
		const [a, b] = q.add([file('a.png'), file('b.png')]);
		q.cancel(b);
		q.cancel(a);
		expect(calls[0].signal.aborted).toBe(true);
		await settle();
		expect(q.items.map((i) => i.status)).toEqual(['cancelled', 'cancelled']);
		expect(calls).toHaveLength(1);
	});

	it('a retry keeps the key and sends the same file again', async () => {
		const { q, calls } = queue();
		const [key] = q.add([file('a.png')]);
		calls[0].reject(
			new ApiError({ type: 'about:blank', title: 'x', status: 413, detail: 'Too big.', code: 'too_large' })
		);
		await settle();
		expect(q.items[0]).toMatchObject({ status: 'failed', error: 'Too big.' });

		q.retry(key);
		expect(calls).toHaveLength(2);
		expect(calls[1].file).toBe(calls[0].file);
		expect(q.items[0]).toMatchObject({ key, status: 'uploading', error: null });
	});

	it('a blocked file is never dispatched', () => {
		const { q, calls } = queue({ preflight: (f) => (f.name.endsWith('.exe') ? 'Not accepted.' : null) });
		const [bad] = q.add([file('a.exe', 'application/octet-stream'), file('b.png')]);
		expect(calls.map((c) => c.file.name)).toEqual(['b.png']);
		expect(q.items[0]).toMatchObject({ status: 'blocked', error: 'Not accepted.' });
		q.retry(bad);
		expect(calls).toHaveLength(1);
	});

	it('passes fields through to every upload', () => {
		const { q, calls } = queue();
		q.add([file('a.png')], { album: 'abc' });
		expect(calls[0].fields).toEqual({ album: 'abc' });
	});

	it('clearing finished items revokes their previews, and only theirs', async () => {
		const { q, calls, revoked } = queue({ concurrency: 1 });
		q.add([file('a.png'), file('b.png'), file('c.mp3', 'audio/mpeg')]);
		calls[0].resolve(response('A'));
		await settle();
		q.clearFinished();
		expect(revoked).toEqual(['blob:1']);
		expect(q.items.map((i) => i.file.name)).toEqual(['b.png', 'c.mp3']);
		expect(q.active).toBe(true);
	});

	it('removing an item cancels it and revokes its preview', () => {
		const { q, calls, revoked } = queue();
		const [a] = q.add([file('a.png')]);
		q.remove(a);
		expect(calls[0].signal.aborted).toBe(true);
		expect(revoked).toEqual(['blob:1']);
		expect(q.items).toEqual([]);
	});
});

describe('resuming chunked uploads', () => {
	it('a retry hands the transport the session it was given', async () => {
		const { store, kept } = memoryStore();
		const { q, calls } = queue({ store });
		const [key] = q.add([file('big.mp4', 'video/mp4')]);
		calls[0].onSession('sess-1');
		await settle();
		expect([...kept.keys()]).toEqual(['sess-1']);
		calls[0].reject(new ApiError({ type: 'about:blank', title: 'x', status: 0, detail: 'offline', code: 'network' }));
		await settle();

		q.retry(key);
		expect(calls[1].session).toBe('sess-1');
		calls[1].resolve(response('B'));
		await settle();
		expect(kept.size).toBe(0);
	});

	it('after a reload, re-selected files that match pick up their sessions', async () => {
		const { store, kept } = memoryStore();
		const big = new File(['x'], 'big.mp4', { type: 'video/mp4', lastModified: 1000 });
		kept.set('sess-9', { session: 'sess-9', name: 'big.mp4', size: 1, lastModified: 1000, fields: { album: 'a' } });
		const { q, calls } = queue({ store });
		expect((await q.interrupted()).map((p) => p.session)).toEqual(['sess-9']);

		const other = new File(['x'], 'big.mp4', { type: 'video/mp4', lastModified: 2000 });
		expect(await q.resume([other])).toBe(0);
		expect(await q.resume([big])).toBe(1);
		expect(calls[0].session).toBe('sess-9');
		expect(calls[0].fields).toEqual({ album: 'a' });
		// Queued now, so not offered again.
		expect(await q.interrupted()).toEqual([]);
	});
});
