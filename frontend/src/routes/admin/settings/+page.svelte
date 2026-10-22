<script lang="ts">
	import { createQuery, useQueryClient } from '@tanstack/svelte-query';
	import { ApiError, admin } from '$lib/api/client';
	import { keys } from '$lib/api/queries';

	const client = useQueryClient();
	const settings = createQuery(() => ({ queryKey: ['settings'], queryFn: admin.settings }));
	let edits = $state<Record<string, string>>({});
	let errors = $state<Record<string, string[]>>({});
	let notice = $state<string | null>(null);
	let filter = $state('');

	// The watermark image: a cache-busting stamp so a new one shows at once.
	let markVersion = $state(Date.now());
	let markMissing = $state(false);
	let markNote = $state<string | null>(null);
	async function setMark(files: FileList | null) {
		const file = files?.[0];
		if (!file) return;
		try {
			await admin.setWatermark(file);
			markMissing = false;
			markVersion = Date.now();
			markNote = 'Watermark image set. Turn on watermark.enabled below to stamp uploads.';
		} catch (e) {
			markNote = e instanceof ApiError ? e.message : 'Setting the watermark failed.';
		}
	}
	async function removeMark() {
		await admin.removeWatermark();
		markMissing = true;
		markNote = 'Watermark image removed.';
	}

	const shown = $derived(
		(settings.data ?? []).filter((s) => !filter || s.key.includes(filter.trim().toLowerCase()))
	);
	const changed = $derived(
		Object.entries(edits).filter(([key, value]) => settings.data?.find((s) => s.key === key)?.value !== value)
	);

	async function save() {
		try {
			const saved = await admin.updateSettings(Object.fromEntries(changed));
			client.setQueryData(['settings'], saved);
			await client.invalidateQueries({ queryKey: keys.config });
			edits = {};
			errors = {};
			notice = 'Saved, and live now.';
		} catch (e) {
			errors = e instanceof ApiError ? (e.problem.errors ?? {}) : {};
			notice = e instanceof ApiError ? e.message : 'Saving failed.';
		}
	}
</script>

<svelte:head><title>Settings</title></svelte:head>

<div class="flex flex-wrap items-center gap-3">
	<h1 class="font-display text-3xl font-bold tracking-[-0.035em]">Settings</h1>
	<input
		type="search"
		placeholder="Filter"
		aria-label="Filter settings"
		bind:value={filter}
		class="field"
	/>
	<button
		type="button"
		class="ml-auto btn btn-accent"
		disabled={changed.length === 0}
		onclick={save}>Save {changed.length || ''} {changed.length === 1 ? 'change' : 'changes'}</button
	>
</div>
<section class="card grid gap-2 p-5" aria-labelledby="mark-title">
	<h2 id="mark-title" class="font-medium">Watermark image</h2>
	<p class="text-sm muted">A still image, ideally a PNG with transparency, at most 5 MiB. The watermark.* settings say where and how strongly it is stamped.</p>
	{#if !markMissing}
		<img
			src="{admin.watermarkUrl}?v={markVersion}"
			alt="The current watermark"
			class="max-h-24 w-fit rounded bg-surface-raised p-2"
			onerror={() => (markMissing = true)}
			data-testid="watermark-preview"
		/>
	{:else}
		<p class="text-sm">No watermark image is set.</p>
	{/if}
	<div class="flex flex-wrap gap-3">
		<label class="btn btn-small cursor-pointer">
			Choose an image
			<input class="sr-only" type="file" accept="image/*" data-testid="watermark-input" onchange={(e) => setMark(e.currentTarget.files)} />
		</label>
		{#if !markMissing}
			<button type="button" class="text-sm underline" onclick={removeMark}>Remove</button>
		{/if}
	</div>
	{#if markNote}<p class="text-sm" role="status">{markNote}</p>{/if}
</section>

<p class="text-sm muted">A save is all or nothing. Settings pinned by the environment are read-only here.</p>
{#if notice}<p role="status">{notice}</p>{/if}
<div class="-mx-4 overflow-x-auto px-4 md:mx-0 md:px-0">
<table class="w-full text-sm">
	<thead><tr class="text-left muted"><th class="py-2">Setting</th><th>Value</th><th>Default</th></tr></thead>
	<tbody>
		{#each shown as setting (setting.key)}
			<tr class="border-t border-line align-top">
				<td class="py-2 pr-3 font-mono">{setting.key}</td>
				<td class="py-1 pr-3">
					<input
						aria-label={setting.key}
						class="w-full field min-h-10 py-1 disabled:opacity-60"
						value={edits[setting.key] ?? setting.value}
						disabled={setting.pinned}
						oninput={(e) => (edits[setting.key] = e.currentTarget.value)}
						data-testid="setting-{setting.key}"
					/>
					{#if setting.pinned}<span class="text-xs muted">pinned by the environment</span>{/if}
					{#if errors[setting.key]}<span class="text-xs text-danger">{errors[setting.key].join(' ')}</span>{/if}
				</td>
				<td class="py-2 font-mono muted">{setting.default}</td>
			</tr>
		{/each}
	</tbody>
</table>
</div>
