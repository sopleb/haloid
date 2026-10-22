<script lang="ts">
	import { resolve } from '$app/paths';
	import { onMount } from 'svelte';
	import type { FileView } from '$lib/api/types';
	import Player from './Player.svelte';

	let {
		file,
		index,
		count,
		onclose,
		onmove
	}: {
		file: FileView;
		index: number;
		count: number;
		onclose: () => void;
		onmove: (index: number) => void;
	} = $props();

	let dialog = $state<HTMLElement>();

	function onkeydown(event: KeyboardEvent) {
		const keys: Record<string, () => void> = {
			Escape: onclose,
			ArrowLeft: () => index > 0 && onmove(index - 1),
			ArrowRight: () => index < count - 1 && onmove(index + 1),
			Home: () => onmove(0),
			End: () => onmove(count - 1)
		};
		const action = keys[event.key];
		if (action && !(event.target instanceof HTMLInputElement)) {
			event.preventDefault();
			action();
		}
	}

	onMount(() => {
		dialog?.focus();
		// The page behind does not scroll while the lightbox is up.
		const overflow = document.body.style.overflow;
		document.body.style.overflow = 'hidden';
		return () => (document.body.style.overflow = overflow);
	});
</script>

<svelte:window {onkeydown} />

<div
	bind:this={dialog}
	class="fixed inset-0 z-20 grid grid-rows-[auto_1fr_auto] bg-black/90 text-white outline-none"
	role="dialog"
	aria-modal="true"
	aria-label={file.title ?? 'File'}
	tabindex="-1"
	data-testid="lightbox"
>
	<header class="flex items-center gap-4 p-3 text-sm">
		<span aria-live="polite">{index + 1} of {count}</span>
		<span class="min-w-0 flex-1 truncate">{file.title ?? ''}</span>
		<a class="underline" href={resolve('/f/[id]/[[slug]]', { id: file.id })} data-sveltekit-reload>Open page</a>
		<button type="button" class="rounded px-2 py-1 hover:bg-white/10" onclick={onclose} aria-label="Close">✕</button>
	</header>
	<div class="grid min-h-0 place-items-center p-2">
		{#key file.id}<Player {file} fill />{/key}
	</div>
	<footer class="flex justify-between p-3">
		<button
			type="button"
			class="rounded px-3 py-2 hover:bg-white/10 disabled:opacity-30"
			disabled={index === 0}
			onclick={() => onmove(index - 1)}>← Previous</button
		>
		<button
			type="button"
			class="rounded px-3 py-2 hover:bg-white/10 disabled:opacity-30"
			disabled={index >= count - 1}
			onclick={() => onmove(index + 1)}>Next →</button
		>
	</footer>
</div>
