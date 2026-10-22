<script lang="ts">
	import type { FileView } from '$lib/api/types';
	import {
		embedAll,
		formatLabels,
		formats,
		targetLabels,
		targets,
		type Format,
		type Target
	} from '$lib/media/embed';
	import CopyButton from './CopyButton.svelte';

	let { files }: { files: FileView[] } = $props();
	let format = $state<Format>('url');
	let target = $state<Target>('image');
	const code = $derived(embedAll(files, format, target));
	const id = $props.id();
	const select = 'field min-h-10 w-auto rounded-full border-0 bg-surface-high py-1.5 pl-4 text-sm font-medium';
</script>

<section class="card grid w-full gap-3 p-4" aria-labelledby="{id}-title">
	<div class="flex flex-wrap items-center gap-2 sm:flex-nowrap">
		<h2 id="{id}-title" class="mr-auto w-full sm:w-auto font-display text-xl font-bold tracking-[-0.03em]">Share</h2>
		<label class="flex items-center gap-2 text-sm muted">
			<span class="sr-only">Format</span>
			<select bind:value={format} class={select} aria-label="Format">
				{#each formats as option (option)}
					<option value={option}>{formatLabels[option]}</option>
				{/each}
			</select>
		</label>
		<label class="flex items-center gap-2 text-sm muted">
			<span class="sr-only">Size</span>
			<select bind:value={target} class={select} aria-label="Size">
				{#each targets as option (option)}
					<option value={option}>{targetLabels[option]}</option>
				{/each}
			</select>
		</label>
	</div>
	<div class="flex items-end gap-2 rounded-[1.1rem] bg-surface p-2 pl-4">
		<textarea
			readonly
			wrap="off"
			rows={Math.min(files.length, 6)}
			class="min-w-0 flex-1 resize-none self-center bg-transparent py-1.5 text-[0.9375rem] font-medium focus:outline-none"
			aria-label="Embed code"
			data-testid="embed-code"
			value={code}
			onfocus={(e) => e.currentTarget.select()}
		></textarea>
		<CopyButton text={code} accent />
	</div>
</section>
