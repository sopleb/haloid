<script lang="ts">
	import { goto } from '$app/navigation';
	import { resolve } from '$app/paths';
	import { ApiError, deleteFile, editAlbum, recordView, userAlbums } from '$lib/api/client';
	import { meQuery } from '$lib/api/queries';
	import type { AlbumView, FileView } from '$lib/api/types';
	import EmbedPanel from '$lib/components/EmbedPanel.svelte';
	import FileEditor from '$lib/components/FileEditor.svelte';
	import Icon from '$lib/components/Icon.svelte';
	import Player from '$lib/components/Player.svelte';
	import { size } from '$lib/upload/preflight';
	import type { PageProps } from './$types';

	let { data }: PageProps = $props();
	// An edit replaces what the loader fetched, until another file loads.
	let edited = $state<FileView | null>(null);
	const file = $derived(edited?.id === data.file.id ? edited : data.file);

	const me = meQuery();
	let albums = $state<AlbumView[]>([]);
	$effect(() => {
		const username = me.data?.username;
		if (file.can_edit && username) {
			userAlbums(username)
				.then((page) => (albums = page.items))
				.catch(() => (albums = []));
		}
	});

	const when = (iso: string) =>
		new Date(iso).toLocaleDateString(undefined, { day: 'numeric', month: 'long', year: 'numeric' });
	const length = (ms: number) => {
		const seconds = Math.round(ms / 1000);
		return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, '0')}`;
	};

	// One view per page visit; the server ignores repeats and the owner.
	$effect(() => {
		recordView(data.file.id).catch(() => {});
	});

	let coverNote = $state<string | null>(null);
	async function makeCover() {
		if (!file.album) return;
		try {
			await editAlbum(file.album, { cover: file.id });
			coverNote = 'This is now the album\'s cover.';
		} catch (e) {
			coverNote = e instanceof ApiError ? e.message : 'Setting the cover failed.';
		}
	}

	let editing = $state(false);
	let deleteError = $state<string | null>(null);
	async function remove() {
		if (!confirm('Delete this file? It stops loading everywhere it was shared.')) return;
		try {
			await deleteFile(file.id);
			await goto(resolve('/'));
		} catch (e) {
			deleteError = e instanceof ApiError ? e.message : 'Deleting failed.';
		}
	}
</script>

<svelte:head><title>{file.title ?? 'Haloid'}</title></svelte:head>

<article class="grid gap-6 lg:grid-cols-[minmax(0,1.6fr)_minmax(20rem,1fr)] lg:items-start lg:gap-10">
	<div class="-mx-4 -mt-4 grid place-items-center bg-surface-raised md:mx-0 md:mt-0 md:rounded-card md:p-3">
		<Player {file} class="w-full md:rounded-[1rem]" />
	</div>

	<div class="grid gap-5">
		<header class="grid gap-2">
			<h1 class="display text-[2.6rem] md:text-5xl" data-testid={file.title ? 'viewer-title' : undefined}>
				{file.title ?? 'Untitled'}
			</h1>
			<p class="muted">
				{#if file.owner}by <a class="font-medium text-ink hover:underline" href={resolve('/u/[username]', { username: file.owner })}>{file.owner}</a> ·{/if}
				<time datetime={file.created_at}>{when(file.created_at)}</time>
				· <span data-testid="views">{file.views.toLocaleString()} {file.views === 1 ? 'view' : 'views'}</span>
			</p>
			{#if file.description}<p class="text-[1.0625rem]">{file.description}</p>{/if}
			{#if file.tags.length}
				<ul class="mt-1 flex flex-wrap gap-2 text-sm" aria-label="Tags">
					{#each file.tags as tag (tag)}<li class="rounded-full bg-surface-raised px-3 py-1 font-medium">{tag}</li>{/each}
				</ul>
			{/if}
		</header>

		<EmbedPanel files={[file]} />

		<dl class="grid grid-cols-2 gap-2">
			<div class="card px-4 py-3.5">
				<dt class="sr-only">Size</dt>
				<dd class="font-display text-2xl font-extrabold tracking-[-0.02em] tabular-nums">{size(file.size)}</dd>
				<dd class="text-sm muted">{file.extension.toUpperCase()}</dd>
			</div>
			<div class="card px-4 py-3.5">
				{#if file.kind === 'audio' && file.duration_ms}
					<dt class="sr-only">Length</dt>
					<dd class="font-display text-2xl font-extrabold tracking-[-0.02em] tabular-nums">{length(file.duration_ms)}</dd>
					<dd class="text-sm muted">long</dd>
				{:else}
					<dt class="sr-only">Dimensions</dt>
					<dd class="font-display text-2xl font-extrabold tracking-[-0.02em] whitespace-nowrap tabular-nums">{file.width} × {file.height}</dd>
					<dd class="text-sm muted">{file.kind === 'video' && file.duration_ms ? `pixels, ${length(file.duration_ms)}` : 'pixels'}</dd>
				{/if}
			</div>
		</dl>

		<div class="grid grid-cols-3 gap-2">
			{#if file.urls.image}
				<!-- eslint-disable-next-line svelte/no-navigation-without-resolve -- the file itself, on its storage -->
				<a class="card grid h-16 content-center justify-items-center gap-0.5 text-sm font-medium hover:bg-surface-high" href={file.urls.image} download>
					<Icon name="download" />Save
				</a>
			{/if}
			{#if file.can_edit}
				<button type="button" class="card grid h-16 content-center justify-items-center gap-0.5 text-sm font-medium hover:bg-surface-high" aria-expanded={editing} onclick={() => (editing = !editing)}>
					<Icon name={editing ? 'x' : 'edit'} />{editing ? 'Close' : 'Edit'}
				</button>
				<button type="button" class="card grid h-16 content-center justify-items-center gap-0.5 text-sm font-medium text-danger hover:bg-surface-high" onclick={remove}>
					<Icon name="trash" />Delete
				</button>
			{/if}
		</div>
		{#if file.can_edit && file.album}
			<button type="button" class="btn btn-small justify-self-start" onclick={makeCover}>Make album cover</button>
			{#if coverNote}<p class="text-sm" role="status">{coverNote}</p>{/if}
		{/if}
		{#if deleteError}<p class="text-danger" role="alert">{deleteError}</p>{/if}
		{#if editing}
			<div class="card p-5">
				<FileEditor
					{file}
					{albums}
					onsaved={(saved) => {
						edited = saved;
						editing = false;
					}}
				/>
			</div>
		{/if}
	</div>
</article>
