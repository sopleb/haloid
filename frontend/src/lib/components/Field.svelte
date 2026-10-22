<script lang="ts">
	import type { HTMLInputAttributes } from 'svelte/elements';

	let {
		label,
		value = $bindable(''),
		errors = [],
		...rest
	}: { label: string; value?: string; errors?: string[] } & HTMLInputAttributes = $props();
	const id = $props.id();
</script>

<div class="grid gap-1">
	<label for={id} class="text-sm font-medium">{label}</label>
	<input
		{id}
		bind:value
		aria-invalid={errors.length > 0}
		aria-describedby={errors.length ? `${id}-errors` : undefined}
		class="field"
		{...rest}
	/>
	{#if errors.length}
		<p id="{id}-errors" class="text-sm text-danger">{errors.join(' ')}</p>
	{/if}
</div>
