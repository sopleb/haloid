// Every server read the app makes goes through these. Route components use
// them; leaf components take plain props and never fetch.
import { createInfiniteQuery, createMutation, createQuery, useQueryClient } from '@tanstack/svelte-query';
import * as api from './client';
import type { FilePage } from './types';

export const keys = {
	me: ['me'] as const,
	config: ['config'] as const,
	file: (id: string) => ['file', id] as const,
	explore: ['explore'] as const,
	user: (name: string) => ['user', name] as const,
	userFiles: (name: string) => ['user', name, 'files'] as const,
	userAlbums: (name: string) => ['user', name, 'albums'] as const,
	album: (id: string) => ['album', id] as const,
	albumFiles: (id: string) => ['album', id, 'files'] as const
};

/** Who is signed in; `null` for a guest rather than an error. */
export function meQuery() {
	return createQuery(() => ({
		queryKey: keys.me,
		queryFn: () =>
			api.getMe().catch((e) => {
				if (e instanceof api.ApiError && e.problem.status === 401) return null;
				throw e;
			}),
		staleTime: 60_000
	}));
}

export function configQuery() {
	return createQuery(() => ({ queryKey: keys.config, queryFn: api.getConfig, staleTime: 60_000 }));
}

function pages(key: () => readonly unknown[], fetchPage: (cursor: string | null) => Promise<FilePage>) {
	return createInfiniteQuery(() => ({
		queryKey: key(),
		queryFn: ({ pageParam }) => fetchPage(pageParam),
		initialPageParam: null as string | null,
		getNextPageParam: (last: FilePage) => last.next ?? undefined
	}));
}

export const exploreQuery = () => pages(() => keys.explore, api.explore);
export const userQuery = (name: () => string) =>
	createQuery(() => ({ queryKey: keys.user(name()), queryFn: () => api.getUser(name()) }));
export const userFilesQuery = (name: () => string) =>
	pages(() => keys.userFiles(name()), (cursor) => api.userFiles(name(), cursor));
export const userAlbumsQuery = (name: () => string) =>
	createQuery(() => ({ queryKey: keys.userAlbums(name()), queryFn: () => api.userAlbums(name()) }));
export const albumQuery = (id: () => string) =>
	createQuery(() => ({ queryKey: keys.album(id()), queryFn: () => api.getAlbum(id()) }));
export const albumFilesQuery = (id: () => string) =>
	pages(() => keys.albumFiles(id()), (cursor) => api.albumFiles(id(), cursor));

/** Signing in or out changes who you are and what you may upload. */
function invalidateSession(client: ReturnType<typeof useQueryClient>) {
	return Promise.all([
		client.invalidateQueries({ queryKey: keys.me }),
		client.invalidateQueries({ queryKey: keys.config })
	]);
}

export function loginMutation() {
	const client = useQueryClient();
	return createMutation(() => ({
		mutationFn: api.login,
		onSuccess: () => invalidateSession(client)
	}));
}

export function registerMutation() {
	return createMutation(() => ({ mutationFn: api.register }));
}

export function logoutMutation() {
	const client = useQueryClient();
	return createMutation(() => ({
		mutationFn: api.logout,
		onSuccess: () => invalidateSession(client)
	}));
}
