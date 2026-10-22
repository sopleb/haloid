<script lang="ts">
	let {
		hasMore,
		loading,
		onclick
	}: { hasMore: boolean; loading: boolean; onclick: () => void } = $props();
</script>

{#if hasMore}
	<div class="mt-6 text-center">
		<button
			type="button"
			class="btn"
			disabled={loading}
			{onclick}
		>
			{loading ? 'Loading…' : 'Show more'}
		</button>
	</div>
{/if}
