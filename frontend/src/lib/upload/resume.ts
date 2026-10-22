// Interrupted chunked uploads, remembered across a reload. A `File` cannot
// be stored, so what survives is the session and enough to recognise the
// file again: the honest resume story is "re-select these files", matched
// on name, size and modification time.

export interface Pending {
	session: string;
	name: string;
	size: number;
	lastModified: number;
	fields: Record<string, string>;
}

export interface PendingStore {
	save(pending: Pending): Promise<void>;
	remove(session: string): Promise<void>;
	all(): Promise<Pending[]>;
}

export function matches(pending: Pending, file: Pick<File, 'name' | 'size' | 'lastModified'>): boolean {
	return (
		pending.name === file.name && pending.size === file.size && pending.lastModified === file.lastModified
	);
}

const DB = 'haloid';
const STORE = 'pending-uploads';

function open(): Promise<IDBDatabase> {
	return new Promise((resolve, reject) => {
		const request = indexedDB.open(DB, 1);
		request.onupgradeneeded = () => request.result.createObjectStore(STORE, { keyPath: 'session' });
		request.onsuccess = () => resolve(request.result);
		request.onerror = () => reject(request.error);
	});
}

async function run<T>(mode: IDBTransactionMode, work: (store: IDBObjectStore) => IDBRequest<T>): Promise<T> {
	const db = await open();
	try {
		return await new Promise<T>((resolve, reject) => {
			const request = work(db.transaction(STORE, mode).objectStore(STORE));
			request.onsuccess = () => resolve(request.result);
			request.onerror = () => reject(request.error);
		});
	} finally {
		db.close();
	}
}

/** The browser's IndexedDB; a no-op store where there is none, or it is refused. */
export const browserStore: PendingStore = {
	save: async (pending) => {
		if (typeof indexedDB === 'undefined') return;
		await run('readwrite', (s) => s.put(pending)).catch(() => {});
	},
	remove: async (session) => {
		if (typeof indexedDB === 'undefined') return;
		await run('readwrite', (s) => s.delete(session)).catch(() => {});
	},
	all: async () => {
		if (typeof indexedDB === 'undefined') return [];
		return (await run<Pending[]>('readonly', (s) => s.getAll()).catch(() => [])) ?? [];
	}
};
