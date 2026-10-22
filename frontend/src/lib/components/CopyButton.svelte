<script lang="ts">
	import { copy } from '$lib/clipboard';
	import Icon from './Icon.svelte';

	let {
		text,
		label = 'Copy',
		accent = false,
		quiet = false
	}: { text: string; label?: string; accent?: boolean; quiet?: boolean } = $props();
	let copied = $state(false);

	async function onclick() {
		copied = await copy(text);
		if (copied) setTimeout(() => (copied = false), 1500);
	}
</script>

{#if quiet}
	<button type="button" class="font-medium hover:underline" {onclick}>
		<span aria-live="polite">{copied ? 'Copied' : label}</span>
	</button>
{:else}
	<button
		type="button"
		class="btn btn-small {copied ? 'bg-accent-soft text-accent-soft-ink hover:bg-accent-soft' : accent ? 'btn-accent' : ''}"
		{onclick}
	>
		<Icon name={copied ? 'check' : 'copy'} size={17} />
		<span aria-live="polite">{copied ? 'Copied' : label}</span>
	</button>
{/if}
