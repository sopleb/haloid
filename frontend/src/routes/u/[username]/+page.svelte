<script lang="ts">
	import { resolve } from '$app/paths';
	import { page } from '$app/state';
	import { useQueryClient } from '@tanstack/svelte-query';
	import { SvelteSet } from 'svelte/reactivity';
	import { ApiError, bulk, createAlbum } from '$lib/api/client';
	import { keys, meQuery, userAlbumsQuery, userFilesQuery, userQuery } from '$lib/api/queries';
	import AlbumForm from '$lib/components/AlbumForm.svelte';
	import Gallery from '$lib/components/Gallery.svelte';
	import Icon from '$lib/components/Icon.svelte';
	import MoreButton from '$lib/components/MoreButton.svelte';

	const username = () => page.params.username ?? '';
	const user = userQuery(username);
	const files = userFilesQuery(username);
	const albums = userAlbumsQuery(username);
	const me = meQuery();
	const client = useQueryClient();
	const mine = $derived(me.data?.username === username());

	// Album tiles take turns through the theme's surfaces.
	const tones = ['bg-accent text-accent-ink', 'bg-surface-raised text-ink', 'bg-accent-soft text-accent-soft-ink', 'bg-surface-high text-ink'];

	const selected = new SvelteSet<string>();
	let target = $state('');
	let notice = $state<string | null>(null);
	let creating = $state(false);

	function toggle(id: string) {
		if (selected.has(id)) selected.delete(id);
		else selected.add(id);
	}

	async function refresh() {
		await Promise.all([
			client.invalidateQueries({ queryKey: keys.user(username()) }),
			client.invalidateQueries({ queryKey: keys.userAlbums(username()) })
		]);
	}

	async function act(action: 'move' | 'delete') {
		if (action === 'delete' && !confirm(`Delete ${selected.size} files?`)) return;
		try {
			const result = await bulk(action, [...selected], target || null);
			notice =
				result.failed.length === 0
					? `${action === 'move' ? 'Moved' : 'Deleted'} ${result.done.length} files.`
					: `${result.done.length} done; ${result.failed.length} could not be changed.`;
			selected.clear();
			await refresh();
		} catch (e) {
			notice = e instanceof ApiError ? e.message : 'That did not work.';
		}
	}
</script>

<svelte:head><title>{user.data?.display_name ?? username()}</title></svelte:head>

{#if user.isError}
	<p class="py-12 text-center" role="alert">There is no such user here.</p>
{:else if user.data}
	<header class="mb-8 grid gap-5">
		<div class="flex items-end gap-4">
			<span class="grid size-16 shrink-0 place-items-center rounded-full bg-accent-soft font-display text-3xl font-extrabold text-accent-soft-ink md:size-20" aria-hidden="true">
				{(user.data.display_name ?? user.data.username).slice(0, 1).toUpperCase()}
			</span>
			<h1 class="display min-w-0 text-[clamp(3.25rem,15vw,6.5rem)] break-words">{user.data.display_name ?? user.data.username}</h1>
		</div>
		<dl class="flex gap-7">
			<div><dd class="font-display text-2xl font-extrabold tabular-nums">{user.data.file_count}</dd><dt class="text-sm muted">files</dt></div>
			<div><dd class="font-display text-2xl font-extrabold tabular-nums">{user.data.album_count}</dd><dt class="text-sm muted">albums</dt></div>
			<div><dd class="font-display text-2xl font-extrabold">@{user.data.username}</dd><dt class="text-sm muted">since {new Date(user.data.created_at).getFullYear()}</dt></div>
		</dl>
	</header>

	{#if albums.data?.items.length || mine}
		<section class="mb-8" aria-labelledby="albums-title">
			<div class="mb-3 flex items-center gap-4">
				<h2 id="albums-title" class="font-display text-2xl font-bold tracking-[-0.03em]">Albums</h2>
				{#if mine}
					<button type="button" class="btn btn-small ml-auto" onclick={() => (creating = !creating)}>
						<Icon name={creating ? 'x' : 'plus'} size={18} />{creating ? 'Cancel' : 'New album'}
					</button>
				{/if}
			</div>
			{#if creating}
				<div class="card mb-4 max-w-md p-5">
					<AlbumForm
						submitLabel="Create album"
						onsubmit={async (fields) => {
							try {
								await createAlbum(fields);
								creating = false;
								notice = null;
								await refresh();
							} catch (e) {
								notice = e instanceof ApiError ? e.message : 'Creating the album failed.';
							}
						}}
					/>
				</div>
			{/if}
			<ul class="-mx-4 flex snap-x scroll-px-4 gap-2.5 overflow-x-auto px-4 pb-2 md:mx-0 md:scroll-px-0 md:px-0" data-testid="album-list">
				{#each albums.data?.items ?? [] as album, i (album.id)}
					<li class="snap-start">
						<a class="group block w-36" href={resolve('/a/[id]/[[slug]]', { id: album.id })}>
							{#if album.cover}
								<img
									src={album.cover}
									alt=""
									loading="lazy"
									class="h-44 w-full rounded-[1.1rem] object-cover transition-transform group-hover:-translate-y-0.5 motion-reduce:transition-none"
								/>
							{:else}
								<span class="grid h-44 place-items-center rounded-[1.1rem] font-display text-6xl font-extrabold transition-transform group-hover:-translate-y-0.5 motion-reduce:transition-none {tones[i % tones.length]}" aria-hidden="true">
									{album.name.slice(0, 1).toUpperCase()}
								</span>
							{/if}
							<span class="mt-2 block truncate font-medium">{album.name}</span>
							<span class="block text-sm muted">{album.file_count} {album.file_count === 1 ? 'file' : 'files'} · {album.privacy}</span>
						</a>
					</li>
				{/each}
			</ul>
		</section>
	{/if}

	{#if notice}<p class="mb-4" role="status">{notice}</p>{/if}

	{#if mine && selected.size}
		<div class="card sticky top-2 z-10 mb-4 flex flex-wrap items-center gap-3 p-3 shadow-[0_12px_32px_rgb(0_0_0/0.35)]" data-testid="bulk-bar">
			<span>{selected.size} selected</span>
			<label class="flex items-center gap-2 text-sm">
				Move to
				<select bind:value={target} class="field min-h-10 w-auto py-1">
					<option value="">No album</option>
					{#each albums.data?.items ?? [] as album (album.id)}
						<option value={album.id}>{album.name}</option>
					{/each}
				</select>
			</label>
			<button type="button" class="btn btn-small" onclick={() => act('move')}>Move</button>
			<button type="button" class="btn btn-small btn-danger" onclick={() => act('delete')}>
				Delete
			</button>
			<button type="button" class="text-sm muted" onclick={() => selected.clear()}>Clear</button>
		</div>
	{/if}

	{#if files.data}
		<Gallery
			files={files.data.pages.flatMap((p) => p.items)}
			hasMore={files.hasNextPage}
			loadMore={() => files.fetchNextPage()}
			selected={mine ? selected : undefined}
			ontoggle={mine ? toggle : undefined}
		/>
		<MoreButton
			hasMore={files.hasNextPage}
			loading={files.isFetchingNextPage}
			onclick={() => files.fetchNextPage()}
		/>
	{/if}
{/if}
