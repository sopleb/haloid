<script lang="ts">
	import { ApiError, editFile } from '$lib/api/client';
	import type { AlbumView, FileView } from '$lib/api/types';

	let {
		file,
		albums,
		onsaved
	}: { file: FileView; albums: AlbumView[]; onsaved: (file: FileView) => void } = $props();

	// The form starts from the file and is reset when another one arrives.
	let title = $state('');
	let description = $state('');
	let tags = $state('');
	let nsfw = $state(false);
	let album = $state('');
	$effect.pre(() => {
		title = file.title ?? '';
		description = file.description ?? '';
		tags = file.tags.join(', ');
		nsfw = file.nsfw;
		album = file.album ?? '';
	});
	let saving = $state(false);
	let error = $state<string | null>(null);

	async function save(event: SubmitEvent) {
		event.preventDefault();
		saving = true;
		error = null;
		try {
			onsaved(
				await editFile(file.id, {
					title: title.trim() || null,
					description: description.trim() || null,
					tags: tags
						.split(',')
						.map((t) => t.trim())
						.filter(Boolean),
					nsfw,
					album: album || null
				})
			);
		} catch (e) {
			error = e instanceof ApiError ? e.message : 'Saving failed.';
		} finally {
			saving = false;
		}
	}
</script>

<form class="card grid w-full gap-3 p-5" onsubmit={save} aria-label="Edit file">
	<label class="grid gap-1 text-sm">
		Title
		<input bind:value={title} maxlength={255} class="field" />
	</label>
	<label class="grid gap-1 text-sm">
		Description
		<textarea bind:value={description} rows="3" class="field"
		></textarea>
	</label>
	<label class="grid gap-1 text-sm">
		Tags, comma-separated
		<input bind:value={tags} class="field" />
	</label>
	{#if albums.length}
		<label class="grid gap-1 text-sm">
			Album
			<select bind:value={album} class="field">
				<option value="">No album</option>
				{#each albums as option (option.id)}
					<option value={option.id}>{option.name}</option>
				{/each}
			</select>
		</label>
	{/if}
	<label class="flex items-center gap-2 text-sm">
		<input type="checkbox" bind:checked={nsfw} /> Not safe for work
	</label>
	{#if error}<p class="text-danger" role="alert">{error}</p>{/if}
	<div>
		<button
			type="submit"
			class="btn btn-accent"
			disabled={saving}
		>
			{saving ? 'Saving…' : 'Save'}
		</button>
	</div>
</form>
