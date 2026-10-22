<script lang="ts">
	import { goto } from '$app/navigation';
	import { resolve } from '$app/paths';
	import { page } from '$app/state';
	import { useQueryClient } from '@tanstack/svelte-query';
	import { ApiError, deleteAlbum, editAlbum, reorderAlbum, unlockAlbum } from '$lib/api/client';
	import { albumFilesQuery, albumQuery, keys, meQuery } from '$lib/api/queries';
	import AlbumForm from '$lib/components/AlbumForm.svelte';
	import Arrange from '$lib/components/Arrange.svelte';
	import EmbedPanel from '$lib/components/EmbedPanel.svelte';
	import Field from '$lib/components/Field.svelte';
	import Gallery from '$lib/components/Gallery.svelte';

	const id = () => page.params.id ?? '';
	const album = albumQuery(id);
	const files = albumFilesQuery(id);
	const me = meQuery();
	const client = useQueryClient();
	const listed = $derived(files.data?.pages.flatMap((p) => p.items) ?? []);
	const locked = $derived(album.error instanceof ApiError && album.error.problem.code === 'album_locked');

	let mode = $state<'view' | 'edit' | 'arrange'>('view');
	let error = $state<string | null>(null);
	let password = $state('');

	async function refresh() {
		await Promise.all([
			client.invalidateQueries({ queryKey: keys.album(id()) }),
			client.invalidateQueries({ queryKey: keys.albumFiles(id()) })
		]);
	}

	async function unlock(event: SubmitEvent) {
		event.preventDefault();
		try {
			await unlockAlbum(id(), password);
			password = '';
			error = null;
			await refresh();
		} catch (e) {
			error =
				e instanceof ApiError && e.problem.status === 401
					? 'That is not the password.'
					: e instanceof ApiError
						? e.message
						: 'Unlocking failed.';
		}
	}

	async function remove(contents: 'keep' | 'delete') {
		const question =
			contents === 'keep'
				? 'Delete this album? Its files stay, outside any album.'
				: 'Delete this album and every file in it?';
		if (!confirm(question)) return;
		try {
			await deleteAlbum(id(), contents);
			const owner = me.data?.username;
			await goto(owner ? resolve('/u/[username]', { username: owner }) : resolve('/'));
		} catch (e) {
			error = e instanceof ApiError ? e.message : 'Deleting the album failed.';
		}
	}
</script>

<svelte:head><title>{album.data?.name ?? 'Album'}</title></svelte:head>

{#if locked}
	<form class="mx-auto grid max-w-sm gap-4 py-12" onsubmit={unlock} data-testid="unlock-form">
		<h1 class="font-display text-3xl font-bold tracking-[-0.035em]">This album has a password</h1>
		<Field label="Password" type="password" autocomplete="off" required bind:value={password} />
		{#if error}<p class="text-danger" role="alert">{error}</p>{/if}
		<button class="btn btn-accent">Open album</button>
	</form>
{:else if album.isError}
	<p class="py-12 text-center" role="alert">There is no such album here.</p>
{:else if album.data}
	<header class="mb-6 grid gap-2">
		<h1 class="display text-5xl md:text-6xl" data-testid="album-name">{album.data.name}</h1>
		{#if album.data.description}<p class="muted">{album.data.description}</p>{/if}
		<p class="text-sm muted">{album.data.file_count} {album.data.file_count === 1 ? 'file' : 'files'} · {album.data.privacy}</p>
		{#if album.data.can_edit}
			<div class="flex flex-wrap gap-3">
				<button type="button" class="btn btn-small" onclick={() => (mode = mode === 'edit' ? 'view' : 'edit')}>
					{mode === 'edit' ? 'Close editor' : 'Edit album'}
				</button>
				{#if listed.length > 1}
					<button type="button" class="btn btn-small" onclick={() => (mode = 'arrange')}>Arrange</button>
				{/if}
				<button type="button" class="btn btn-small" onclick={() => remove('keep')}>
					Delete album, keep files
				</button>
				<button type="button" class="btn btn-small btn-danger" onclick={() => remove('delete')}>
					Delete album and files
				</button>
			</div>
		{/if}
		{#if error}<p class="text-danger" role="alert">{error}</p>{/if}
	</header>
	{#if mode === 'edit'}
		<div class="card mb-6 max-w-md p-5">
			<AlbumForm
				album={album.data}
				submitLabel="Save"
				onsubmit={async (fields) => {
					try {
						const saved = await editAlbum(id(), fields);
						client.setQueryData(keys.album(id()), saved);
						mode = 'view';
						error = null;
					} catch (e) {
						error = e instanceof ApiError ? e.message : 'Saving failed.';
					}
				}}
			/>
		</div>
	{/if}
	{#if mode === 'arrange'}
		<Arrange
			files={listed}
			oncancel={() => (mode = 'view')}
			onsave={async (ids) => {
				try {
					await reorderAlbum(id(), ids);
					await refresh();
					mode = 'view';
				} catch (e) {
					error = e instanceof ApiError ? e.message : 'Saving the order failed.';
				}
			}}
		/>
	{:else if files.data}
		<Gallery files={listed} hasMore={files.hasNextPage} loadMore={() => files.fetchNextPage()} />
		<div class="mt-6 text-center">
			{#if files.hasNextPage}
				<button
					type="button"
					class="btn"
					disabled={files.isFetchingNextPage}
					onclick={() => files.fetchNextPage()}>{files.isFetchingNextPage ? 'Loading…' : 'Show more'}</button
				>
			{/if}
		</div>
		{#if listed.length}
			<div class="mt-8"><EmbedPanel files={listed} /></div>
		{/if}
	{/if}
{/if}
