<script lang="ts">
	import '../app.css';
	import { page } from '$app/state';
	import { QueryClient, QueryClientProvider } from '@tanstack/svelte-query';
	import UploadDock from '$lib/components/UploadDock.svelte';
	import Footer from './Footer.svelte';
	import Nav from './Nav.svelte';
	let { children } = $props();

	const client = new QueryClient({
		defaultOptions: {
			// A 4xx will not come right by asking again.
			queries: { retry: (count, error) => count < 2 && !(error as { problem?: { status: number } }).problem }
		}
	});
</script>

<QueryClientProvider {client}>
	<div class="flex min-h-dvh flex-col">
		<header>
			<Nav />
		</header>
		<main class="mx-auto w-full max-w-6xl flex-1 px-4 pt-4 pb-12 md:px-6 md:pt-8">
			{@render children()}
		</main>
		<Footer />
		<!-- The home page lists uploads in place; everywhere else they float. -->
		{#if page.url.pathname !== '/'}<UploadDock floating />{/if}
	</div>
</QueryClientProvider>
