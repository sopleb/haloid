<script lang="ts">
	import { exploreQuery } from '$lib/api/queries';
	import Gallery from '$lib/components/Gallery.svelte';
	import MoreButton from '$lib/components/MoreButton.svelte';

	const files = exploreQuery();
</script>

<svelte:head><title>Explore</title></svelte:head>

<h1 class="display mb-8 text-6xl md:text-7xl">Explore</h1>
{#if files.isError}
	<p class="text-danger" role="alert">{files.error.message}</p>
{:else if files.data}
	<Gallery
		files={files.data.pages.flatMap((p) => p.items)}
		hasMore={files.hasNextPage}
		loadMore={() => files.fetchNextPage()}
	/>
	<MoreButton
		hasMore={files.hasNextPage}
		loading={files.isFetchingNextPage}
		onclick={() => files.fetchNextPage()}
	/>
{/if}
