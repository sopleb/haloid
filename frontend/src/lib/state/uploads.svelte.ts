// The upload queue: a module-scoped singleton, so a navigation does not stop
// the bytes. Several files at once, a few in flight, each with its own
// progress, cancel and retry.
//
// Progress events arrive far faster than anyone can read them; writing
// each into reactive state would re-render a long queue to death. They land
// in a plain scratch map instead and are flushed once per frame.
import { ApiError, sessions, upload, type UploadOptions } from '$lib/api/client';
import type { FileView, UploadResponse } from '$lib/api/types';
import { CHUNKED_ABOVE, uploadInChunks } from '$lib/upload/chunked';
import { browserStore, matches, type Pending, type PendingStore } from '$lib/upload/resume';

export type Status = 'blocked' | 'queued' | 'uploading' | 'done' | 'failed' | 'cancelled';

export interface UploadItem {
	/** Ours, stable across retries. */
	key: number;
	file: File;
	status: Status;
	/** 0 to 1. */
	progress: number;
	error: string | null;
	result: FileView | null;
	duplicate: boolean;
	/** An object URL for an image's preview, revoked with the item. */
	preview: string | null;
	fields: Record<string, string>;
	/** A chunked upload's server session, kept so a retry resumes it. */
	session: string | null;
}

export type Transport = (
	file: File,
	options: Required<Pick<UploadOptions, 'onProgress' | 'signal'>> &
		Pick<UploadOptions, 'fields'> & {
			session: string | null;
			onSession: (id: string) => void;
		}
) => Promise<UploadResponse>;

/** One request for small files, resumable chunks for big ones. */
export const defaultTransport: Transport = (file, options) =>
	file.size > CHUNKED_ABOVE ? uploadInChunks(file, sessions, options) : upload(file, options);

export interface QueueOptions {
	concurrency?: number;
	/** Why a file cannot go, or `null`. A blocked file is never sent. */
	preflight?: (file: File) => string | null;
	createPreview?: (file: File) => string | null;
	revokePreview?: (url: string) => void;
	/** How progress is flushed; a frame by default. */
	schedule?: (flush: () => void) => void;
	/** Where interrupted chunked uploads are remembered across a reload. */
	store?: PendingStore;
}

const finished: Status[] = ['done', 'failed', 'cancelled', 'blocked'];

function message(error: unknown): string {
	if (error instanceof ApiError) return error.message;
	return 'The upload failed.';
}

export class UploadQueue {
	items = $state<UploadItem[]>([]);

	#transport: Transport;
	#options: Required<QueueOptions>;
	#nextKey = 1;
	// Deliberately not reactive: nothing renders the controllers, and the
	// progress scratch exists precisely to keep events out of reactive state.
	// eslint-disable-next-line svelte/prefer-svelte-reactivity
	#controllers = new Map<number, AbortController>();
	// eslint-disable-next-line svelte/prefer-svelte-reactivity
	#progress = new Map<number, number>();
	#flushPending = false;

	constructor(transport: Transport, options: QueueOptions = {}) {
		this.#transport = transport;
		this.#options = {
			concurrency: options.concurrency ?? 3,
			preflight: options.preflight ?? (() => null),
			createPreview:
				options.createPreview ??
				((file) => (file.type.startsWith('image/') ? URL.createObjectURL(file) : null)),
			revokePreview: options.revokePreview ?? ((url) => URL.revokeObjectURL(url)),
			store: options.store ?? browserStore,
			schedule:
				options.schedule ??
				((flush) =>
					typeof requestAnimationFrame === 'function' ? requestAnimationFrame(flush) : setTimeout(flush, 16))
		};
	}

	/** Replace the pre-flight check, as `/config` arrives or changes. */
	setPreflight(preflight: (file: File) => string | null) {
		this.#options.preflight = preflight;
	}

	get active(): boolean {
		return this.items.some((item) => item.status === 'queued' || item.status === 'uploading');
	}

	add(files: Iterable<File>, fields: Record<string, string> = {}, session: string | null = null): number[] {
		const keys: number[] = [];
		for (const file of files) {
			const blocked = this.#options.preflight(file);
			const key = this.#nextKey++;
			keys.push(key);
			this.items.push({
				key,
				file,
				status: blocked ? 'blocked' : 'queued',
				progress: 0,
				error: blocked,
				result: null,
				duplicate: false,
				preview: this.#options.createPreview(file),
				fields,
				session
			});
		}
		this.#pump();
		return keys;
	}

	#find(key: number): UploadItem | undefined {
		return this.items.find((item) => item.key === key);
	}

	cancel(key: number) {
		const item = this.#find(key);
		if (!item) return;
		if (item.status === 'uploading') {
			this.#controllers.get(key)?.abort();
		} else if (item.status === 'queued') {
			item.status = 'cancelled';
		}
	}

	/** Send a failed or cancelled file again, under the same key. */
	retry(key: number) {
		const item = this.#find(key);
		if (!item || (item.status !== 'failed' && item.status !== 'cancelled')) return;
		item.status = 'queued';
		item.progress = 0;
		item.error = null;
		this.#pump();
	}

	remove(key: number) {
		this.cancel(key);
		const item = this.#find(key);
		if (item?.preview) this.#options.revokePreview(item.preview);
		if (item?.session) void this.#options.store.remove(item.session);
		this.items = this.items.filter((i) => i.key !== key);
	}

	/** Interrupted uploads remembered from before a reload, not in the queue. */
	async interrupted(): Promise<Pending[]> {
		const queued = this.items.map((item) => item.session).filter(Boolean);
		return (await this.#options.store.all()).filter((p) => !queued.includes(p.session));
	}

	/**
	 * Re-selected files: each one that matches an interrupted upload carries
	 * on from where it stopped. Returns how many matched.
	 */
	async resume(files: Iterable<File>): Promise<number> {
		const pending = await this.interrupted();
		let matched = 0;
		for (const file of files) {
			const found = pending.find((p) => matches(p, file));
			if (!found) continue;
			matched += 1;
			this.add([file], found.fields, found.session);
		}
		return matched;
	}

	/** Forget an interrupted upload without resuming it. */
	async discard(session: string) {
		await this.#options.store.remove(session);
	}

	/** Drop everything that is over, and the previews with it. */
	clearFinished() {
		for (const item of this.items) {
			if (finished.includes(item.status) && item.preview) this.#options.revokePreview(item.preview);
		}
		this.items = this.items.filter((item) => !finished.includes(item.status));
	}

	#pump() {
		let running = this.items.filter((item) => item.status === 'uploading').length;
		for (const item of this.items) {
			if (running >= this.#options.concurrency) break;
			if (item.status !== 'queued') continue;
			running += 1;
			void this.#send(item.key);
		}
	}

	#flush = () => {
		this.#flushPending = false;
		for (const [key, fraction] of this.#progress) {
			const item = this.#find(key);
			if (item?.status === 'uploading') item.progress = fraction;
		}
		this.#progress.clear();
	};

	async #send(key: number) {
		const item = this.#find(key);
		if (!item) return;
		const controller = new AbortController();
		this.#controllers.set(key, controller);
		item.status = 'uploading';
		try {
			const response = await this.#transport(item.file, {
				signal: controller.signal,
				fields: item.fields,
				session: item.session,
				onSession: (id) => {
					const current = this.#find(key);
					if (current) current.session = id;
					void this.#options.store.save({
						session: id,
						name: item.file.name,
						size: item.file.size,
						lastModified: item.file.lastModified,
						fields: item.fields
					});
				},
				onProgress: (fraction) => {
					this.#progress.set(key, fraction);
					if (!this.#flushPending) {
						this.#flushPending = true;
						this.#options.schedule(this.#flush);
					}
				}
			});
			const done = this.#find(key);
			if (done) {
				this.#progress.delete(key);
				done.status = 'done';
				done.progress = 1;
				done.result = response.file;
				done.duplicate = response.duplicate;
				if (done.session) void this.#options.store.remove(done.session);
			}
		} catch (error) {
			const failed = this.#find(key);
			if (failed) {
				this.#progress.delete(key);
				const aborted = error instanceof DOMException && error.name === 'AbortError';
				failed.status = aborted ? 'cancelled' : 'failed';
				failed.error = aborted ? null : message(error);
			}
		} finally {
			this.#controllers.delete(key);
			this.#pump();
		}
	}
}

/** The app's one queue. */
export const uploads = new UploadQueue(defaultTransport);
