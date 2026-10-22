import type {
	AdminUserPage,
	AdminUserView,
	AuditEntryView,
	LicenceView,
	InstallingView,
	UpdateView,
	QueuePage,
	SettingView,
	StorageBody,
	StorageTestView,
	StorageView,
	AlbumPage,
	ApiKeyView,
	CreatedApiKey,
	LoginSessionView,
	ProfilePatch,
	AlbumPatch,
	AlbumView,
	BulkResult,
	ConfigView,
	FilePage,
	FilePatch,
	FileView,
	MeView,
	NewAlbum,
	OpenSessionBody,
	SessionView,
	Problem,
	UploadResponse,
	UserView
} from './types';

/** Every request goes here: v2 only, never the legacy API. */
const API = '/api/v2';

export class ApiError extends Error {
	constructor(readonly problem: Problem) {
		super(problem.detail || problem.title);
	}
}

/** Turn any failed response into an `ApiError`, problem details or not. */
export async function toError(response: Pick<Response, 'status' | 'statusText' | 'json'>): Promise<ApiError> {
	try {
		return new ApiError((await response.json()) as Problem);
	} catch {
		return new ApiError({
			type: 'about:blank',
			title: response.statusText || 'Error',
			status: response.status,
			detail: `The server answered ${response.status}.`,
			code: 'unexpected'
		});
	}
}

/**
 * One JSON request. Every call carries `X-Requested-With`, which is what
 * lets a cookie-authenticated write past the server's CSRF check — a
 * cross-site form cannot set it — and the session cookie rides along
 * because the API is same-origin.
 */
async function request<T>(
	method: 'GET' | 'POST' | 'PATCH' | 'PUT' | 'DELETE',
	path: string,
	body?: unknown,
	fetcher: typeof fetch = fetch,
	headers: Record<string, string> = {}
): Promise<T> {
	const response = await fetcher(`${API}${path}`, {
		method,
		credentials: 'same-origin',
		headers: {
			accept: 'application/json',
			'x-requested-with': 'XMLHttpRequest',
			...(body === undefined ? {} : { 'content-type': 'application/json' }),
			...headers
		},
		body: body === undefined ? undefined : JSON.stringify(body)
	});
	if (!response.ok) throw await toError(response);
	// 204, and the 202 of a queued purge, have no body to parse.
	const text = response.status === 204 ? '' : await response.text();
	return (text ? JSON.parse(text) : undefined) as T;
}

const segment = encodeURIComponent;

/** A list cursor as a query string, or nothing. */
function page(cursor?: string | null, limit?: number): string {
	const params = new URLSearchParams();
	if (cursor) params.set('cursor', cursor);
	if (limit) params.set('limit', String(limit));
	const query = params.toString();
	return query ? `?${query}` : '';
}

export const getFile = (id: string, fetcher?: typeof fetch) =>
	request<FileView>('GET', `/files/${segment(id)}`, undefined, fetcher);
export const getConfig = () => request<ConfigView>('GET', '/config');
export const getMe = () => request<MeView>('GET', '/auth/me');
export const explore = (cursor?: string | null) => request<FilePage>('GET', `/files${page(cursor, 40)}`);
export const getUser = (username: string) => request<UserView>('GET', `/users/${segment(username)}`);
export const userFiles = (username: string, cursor?: string | null) =>
	request<FilePage>('GET', `/users/${segment(username)}/files${page(cursor, 40)}`);
export const getAlbum = (id: string) => request<AlbumView>('GET', `/albums/${segment(id)}`);
export const albumFiles = (id: string, cursor?: string | null) =>
	request<FilePage>('GET', `/albums/${segment(id)}/files${page(cursor, 40)}`);

export const userAlbums = (username: string, cursor?: string | null) =>
	request<AlbumPage>('GET', `/users/${segment(username)}/albums${page(cursor, 100)}`);

export const editFile = (id: string, patch: FilePatch) =>
	request<FileView>('PATCH', `/files/${segment(id)}`, patch);
/** Delete a file: as its owner, or with the token from its delete link. */
export const deleteFile = (id: string, token?: string) =>
	request<void>('DELETE', `/files/${segment(id)}`, undefined, fetch, token ? { 'x-delete-token': token } : {});
export const bulk = (action: 'move' | 'delete', ids: string[], album?: string | null) =>
	request<BulkResult>('POST', '/files/bulk', { action, ids, ...(action === 'move' ? { album: album ?? null } : {}) });

export const createAlbum = (album: NewAlbum) => request<AlbumView>('POST', '/albums', album);
export const editAlbum = (id: string, patch: AlbumPatch) =>
	request<AlbumView>('PATCH', `/albums/${segment(id)}`, patch);
/** Count a view of a file's page: once per visit, fire and forget. */
export const recordView = (id: string) => request<void>('POST', `/files/${segment(id)}/view`);
export const deleteAlbum = (id: string, contents: 'keep' | 'delete') =>
	request<void>('DELETE', `/albums/${segment(id)}?contents=${contents}`);
export const reorderAlbum = (id: string, files: string[]) =>
	request<void>('PUT', `/albums/${segment(id)}/order`, { files });

export const updateMe = (patch: ProfilePatch) => request<MeView>('PATCH', '/auth/me', patch);
export const changePassword = (current: string, next: string) =>
	request<void>('POST', '/auth/password', { current, new: next });
export const apiKeys = () => request<ApiKeyView[]>('GET', '/auth/api-keys');
export const createApiKey = (label: string) => request<CreatedApiKey>('POST', '/auth/api-keys', { label });
export const revokeApiKey = (id: number) => request<void>('DELETE', `/auth/api-keys/${id}`);
export const loginSessions = () => request<LoginSessionView[]>('GET', '/auth/sessions');
export const revokeSession = (id: string) => request<void>('DELETE', `/auth/sessions/${segment(id)}`);
export const unlockAlbum = (id: string, password: string) =>
	request<void>('POST', `/albums/${segment(id)}/unlock`, { password });

/** Moderation and administration. */
export const admin = {
	queue: (cursor?: string | null) => request<QueuePage>('GET', `/moderation/queue${page(cursor, 50)}`),
	approve: (id: string) => request<void>('POST', `/moderation/${segment(id)}/approve`),
	deny: (id: string, note?: string) =>
		request<void>('POST', `/moderation/${segment(id)}/deny`, note ? { note } : {}),
	settings: () => request<SettingView[]>('GET', '/settings'),
	updateSettings: (changes: Record<string, string>) => request<SettingView[]>('PATCH', '/settings', changes),
	users: (search: string, cursor?: string | null) => {
		const query = new URLSearchParams();
		if (search) query.set('search', search);
		if (cursor) query.set('cursor', cursor);
		const text = query.toString();
		return request<AdminUserPage>('GET', `/admin/users${text ? `?${text}` : ''}`);
	},
	updateUser: (id: string, patch: { role?: string; status?: string }) =>
		request<AdminUserView>('PATCH', `/admin/users/${segment(id)}`, patch),
	storages: () => request<StorageView[]>('GET', '/storages'),
	createStorage: (body: StorageBody) => request<StorageView>('POST', '/storages', body),
	updateStorage: (id: number, body: StorageBody) => request<StorageView>('PATCH', `/storages/${id}`, body),
	retireStorage: (id: number) => request<void>('POST', `/storages/${id}/retire`),
	testStorage: (id: number) => request<StorageTestView>('POST', `/storages/${id}/test`),
	deleted: (cursor?: string | null) => request<FilePage>('GET', `/admin/files/deleted${page(cursor, 50)}`),
	restore: (id: string) => request<void>('POST', `/admin/files/${segment(id)}/restore`),
	purge: (id: string) => request<void>('POST', `/admin/files/${segment(id)}/purge`),
	/** Set the watermark image: the file's bytes as the body. */
	setWatermark: async (file: Blob) => {
		const response = await fetch(`${API}/settings/watermark`, {
			method: 'PUT',
			credentials: 'same-origin',
			headers: { 'x-requested-with': 'XMLHttpRequest', 'content-type': file.type || 'application/octet-stream' },
			body: file
		});
		if (!response.ok) throw await toError(response);
	},
	removeWatermark: () => request<void>('DELETE', '/settings/watermark'),
	licence: () => request<LicenceView>('GET', '/licence'),
	installLicence: (key: string) => request<LicenceView>('PUT', '/licence', { key }),
	removeLicence: () => request<void>('DELETE', '/licence'),
	updates: () => request<UpdateView>('GET', '/updates'),
	checkUpdates: () => request<UpdateView>('POST', '/updates/check'),
	installUpdate: () => request<InstallingView>('POST', '/updates/install'),
	watermarkUrl: `${API}/settings/watermark`,
	audit: (before?: number) =>
		request<AuditEntryView[]>('GET', `/admin/audit${before ? `?before=${before}` : ''}`)
};

export interface Credentials {
	login: string;
	password: string;
	altcha: string;
}

export interface Registration {
	username: string;
	email?: string;
	password: string;
	altcha: string;
}

export const login = (credentials: Credentials) => request<MeView>('POST', '/auth/login', credentials);
export const register = (registration: Registration) =>
	request<MeView>('POST', '/auth/register', registration);
export const logout = () => request<void>('POST', '/auth/logout');

export interface UploadOptions {
	onProgress?: (fraction: number) => void;
	signal?: AbortSignal;
	/** Form fields beside the file: `album`, `nsfw`, `title`, … */
	fields?: Record<string, string>;
}

/**
 * One request with upload progress. XMLHttpRequest rather than fetch,
 * because fetch reports no upload progress in any shipping browser. The
 * `X-Requested-With` header is what lets a cookie-authenticated write past
 * the server's CSRF check: a cross-site form cannot set it.
 */
function send<T>(
	method: 'POST' | 'PUT',
	path: string,
	body: XMLHttpRequestBodyInit | null,
	options: Pick<UploadOptions, 'onProgress' | 'signal'> = {}
): Promise<T> {
	return new Promise((resolve, reject) => {
		const xhr = new XMLHttpRequest();
		xhr.open(method, `${API}${path}`);
		xhr.setRequestHeader('X-Requested-With', 'XMLHttpRequest');
		xhr.responseType = 'json';
		xhr.upload.onprogress = (event) => {
			if (event.lengthComputable) options.onProgress?.(event.loaded / event.total);
		};
		xhr.onload = () => {
			if (xhr.status >= 200 && xhr.status < 300) {
				resolve(xhr.response as T);
			} else {
				const problem = (xhr.response as Problem | null) ?? {
					type: 'about:blank',
					title: 'Upload failed',
					status: xhr.status,
					detail: `The server answered ${xhr.status}.`,
					code: 'unexpected'
				};
				reject(new ApiError(problem));
			}
		};
		xhr.onerror = () =>
			reject(
				new ApiError({
					type: 'about:blank',
					title: 'Network error',
					status: 0,
					detail: 'The upload could not reach the server.',
					code: 'network'
				})
			);
		if (options.signal?.aborted) {
			reject(new DOMException('Upload cancelled', 'AbortError'));
			return;
		}
		options.signal?.addEventListener('abort', () => xhr.abort());
		xhr.onabort = () => reject(new DOMException('Upload cancelled', 'AbortError'));
		xhr.send(body);
	});
}

/** Upload one file in a single request. */
export function upload(file: File, options: UploadOptions = {}): Promise<UploadResponse> {
	const form = new FormData();
	for (const [name, value] of Object.entries(options.fields ?? {})) form.append(name, value);
	form.append('file', file);
	return send<UploadResponse>('POST', '/uploads', form, options);
}

/** Upload from a URL the server fetches, where the instance allows it. */
export function uploadFromUrl(url: string): Promise<UploadResponse> {
	const form = new FormData();
	form.append('url', url);
	return send<UploadResponse>('POST', '/uploads', form);
}

/** The chunked-upload endpoints, for `lib/upload/chunked.ts`. */
export const sessions = {
	open: (body: OpenSessionBody) => request<SessionView>('POST', '/uploads/sessions', body),
	state: (id: string) => request<SessionView>('GET', `/uploads/sessions/${segment(id)}`),
	put: (
		id: string,
		n: number,
		chunk: Blob,
		options: Pick<UploadOptions, 'onProgress' | 'signal'>
	) => send<SessionView>('PUT', `/uploads/sessions/${segment(id)}/chunks/${n}`, chunk, options),
	complete: (id: string, signal?: AbortSignal) =>
		send<UploadResponse>('POST', `/uploads/sessions/${segment(id)}/complete`, null, { signal }),
	abandon: (id: string) => request<void>('DELETE', `/uploads/sessions/${segment(id)}`)
};
