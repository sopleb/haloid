<script lang="ts">
	import { resolve } from '$app/paths';
	import { uploads, type UploadItem } from '$lib/state/uploads.svelte';
	import { size } from '$lib/upload/preflight';
	import CopyButton from './CopyButton.svelte';
	import Icon from './Icon.svelte';

	/** Floating over other pages, rather than in place on the home page. */
	let { floating = false }: { floating?: boolean } = $props();

	const labels: Record<UploadItem['status'], string> = {
		blocked: 'Not sent',
		queued: 'Waiting',
		uploading: 'Uploading',
		done: 'Done',
		failed: 'Failed',
		cancelled: 'Cancelled'
	};
	const finished = $derived(uploads.items.filter((i) => i.status === 'done').length);
</script>

{#if uploads.items.length}
	<section
		class={floating
			? 'fixed inset-x-3 bottom-24 z-10 mx-auto max-h-[45dvh] max-w-xl overflow-y-auto rounded-card bg-surface-raised p-3 shadow-[0_12px_32px_rgb(0_0_0/0.45)] md:bottom-4'
			: 'mt-10'}
		aria-label="Uploads"
		data-testid="upload-dock"
	>
		<header class="mb-3 flex items-baseline justify-between gap-4 {floating ? 'px-1' : ''}">
			<h2 class="display {floating ? 'text-xl' : 'text-2xl'}">Uploads</h2>
			<span class="text-sm muted tabular-nums">{finished} of {uploads.items.length} done</span>
			<button type="button" class="ml-auto text-sm font-medium muted hover:text-ink" onclick={() => uploads.clearFinished()}>
				Clear finished
			</button>
		</header>
		<ul class="grid gap-2">
			{#each uploads.items as item (item.key)}
				<li
					class="grid gap-2 rounded-[1.1rem] p-2.5 {floating ? 'bg-surface-high' : 'bg-surface-raised'}"
					data-testid="upload-item"
					data-status={item.status}
				>
					<div class="flex items-center gap-3.5">
						{#if item.preview}
							<img src={item.preview} alt="" class="size-13 shrink-0 rounded-xl object-cover" />
						{:else}
							<span class="grid size-13 shrink-0 place-items-center rounded-xl bg-surface-high muted"><Icon name="play" /></span>
						{/if}
						<div class="min-w-0 flex-1">
							<p class="truncate font-medium">{item.file.name}</p>
							<p class="text-sm muted tabular-nums">
								{#if item.status === 'uploading'}
									{size(item.file.size * item.progress)} of {size(item.file.size)}
								{:else if item.status === 'done'}
									{size(item.file.size)}{item.duplicate ? ' · uploaded before' : ''}
								{:else}
									{labels[item.status]} · {size(item.file.size)}
								{/if}
							</p>
							{#if item.status === 'uploading'}
								<progress
									class="mt-1.5 block h-1 w-full appearance-none overflow-hidden rounded-full [&::-moz-progress-bar]:bg-accent [&::-webkit-progress-bar]:bg-surface-high [&::-webkit-progress-value]:bg-accent"
									max="1"
									value={item.progress}
									aria-label="{item.file.name} progress"
								></progress>
							{/if}
						</div>
						{#if item.status === 'uploading'}
							<span class="font-display text-lg font-bold text-accent-soft-ink tabular-nums">
								{Math.round(item.progress * 100)}%
							</span>
						{/if}
						{#if item.status === 'queued' || item.status === 'uploading'}
							<button type="button" class="btn btn-small" onclick={() => uploads.cancel(item.key)}>Cancel</button>
						{:else if item.status === 'failed' || item.status === 'cancelled'}
							<button type="button" class="btn btn-small" onclick={() => uploads.retry(item.key)}>
								<Icon name="retry" size={18} />Retry
							</button>
						{/if}
						{#if item.status !== 'uploading'}
							<button
								type="button"
								class="grid size-9 place-items-center rounded-full muted hover:bg-surface-high hover:text-ink"
								aria-label="Remove {item.file.name}"
								onclick={() => uploads.remove(item.key)}><Icon name="x" size={18} /></button
							>
						{/if}
					</div>
					{#if item.error}
						<p class="px-1 text-sm text-danger" role="alert">{item.error}</p>
					{/if}
					{#if item.result}
						<div class="flex flex-wrap items-center gap-x-4 gap-y-1 px-1 text-sm">
							<CopyButton text={item.result.urls.viewer} label="Copy link" accent />
							<a class="font-medium hover:underline" href={resolve('/f/[id]/[[slug]]', { id: item.result.id })}>Open</a>
							{#if item.result.urls.delete}
								<!-- eslint-disable-next-line svelte/no-navigation-without-resolve -- the server builds this absolute URL, which names our own /delete page -->
								<a class="font-medium hover:underline" href={item.result.urls.delete} data-testid="delete-link">Delete link</a>
								<CopyButton text={item.result.urls.delete} label="Copy delete link" quiet />
								<span class="w-full muted">Keep the delete link private: anyone who has it can remove this upload.</span>
							{/if}
						</div>
					{/if}
				</li>
			{/each}
		</ul>
	</section>
{/if}
