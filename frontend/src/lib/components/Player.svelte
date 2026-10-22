<script lang="ts">
	import type { FileView } from '$lib/api/types';
	import { altText } from '$lib/media/embed';
	import { reducedMotion } from '$lib/state/prefs.svelte';

	let { file, fill = false, class: cls = 'rounded-card' }: { file: FileView; fill?: boolean; class?: string } = $props();
	const alt = $derived(altText(file));
	// With less motion asked for, an animation waits for a click.
	let still = $state(reducedMotion());
	const src = $derived(
		file.is_animated && still ? (file.urls.frame ?? file.urls.medium ?? file.urls.image) : file.urls.image
	);
	const sizing = $derived(fill ? 'max-h-[85dvh] max-w-full w-auto h-auto' : 'h-auto max-w-full');
</script>

{#if file.kind === 'image'}
	<img {src} {alt} width={file.width} height={file.height} class="{sizing} {cls}" data-testid="viewer-image" />
	{#if file.is_animated && still}
		<button type="button" class="btn btn-small mt-2" onclick={() => (still = false)}>Play animation</button>
	{/if}
{:else if file.kind === 'video'}
	<!-- svelte-ignore a11y_media_has_caption -->
	<video
		src={file.urls.image}
		poster={file.urls.frame ?? file.urls.medium ?? undefined}
		width={file.width}
		height={file.height}
		controls
		preload="metadata"
		class="{sizing} {cls}"
		data-testid="viewer-video"
	></video>
{:else}
	<figure class="grid w-full max-w-md gap-3">
		{#if file.urls.medium || file.urls.thumb}
			<img src={file.urls.medium ?? file.urls.thumb} alt="" class={cls} />
		{/if}
		<audio src={file.urls.image} controls preload="metadata" class="w-full" data-testid="viewer-audio"></audio>
		{#if file.title}<figcaption class="text-center">{file.title}</figcaption>{/if}
	</figure>
{/if}
