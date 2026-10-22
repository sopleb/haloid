<script lang="ts">
	import { createInfiniteQuery, useQueryClient } from '@tanstack/svelte-query';
	import { ApiError, admin } from '$lib/api/client';
	import type { FilePage } from '$lib/api/types';

	const client = useQueryClient();
	const deleted = createInfiniteQuery(() => ({
		queryKey: ['deleted'],
		queryFn: ({ pageParam }) => admin.deleted(pageParam),
		initialPageParam: null as string | null,
		getNextPageParam: (last: FilePage) => last.next ?? undefined
	}));
	const items = $derived(deleted.data?.pages.flatMap((p) => p.items) ?? []);
	let notice = $state<string | null>(null);

	async function act(id: string, purge: boolean) {
		if (purge && !confirm('Destroy this file for good? This cannot be undone.')) return;
		try {
			if (purge) await admin.purge(id);
			else await admin.restore(id);
			notice = purge ? 'Queued for destruction.' : 'Restored.';
			await client.invalidateQueries({ queryKey: ['deleted'] });
		} catch (e) {
			notice = e instanceof ApiError ? e.message : 'That did not work.';
		}
	}
</script>

<svelte:head><title>Deleted files</title></svelte:head>

<h1 class="font-display text-3xl font-bold tracking-[-0.035em]">Deleted files</h1>
<p class="text-sm muted">Deleted files are kept, out of reach, until an administrator purges them.</p>
{#if notice}<p role="status">{notice}</p>{/if}
<ul class="grid gap-2" data-testid="deleted-files">
	{#each items as file (file.id)}
		<li class="flex flex-wrap items-center gap-3 rounded-[1.1rem] bg-surface-raised p-3 text-sm" data-testid="deleted-file">
			<code>{file.id}</code>
			<span class="min-w-0 flex-1 truncate">{file.title ?? `${file.kind}, ${file.extension}`}</span>
			<span class="muted">{new Date(file.created_at).toLocaleDateString()}</span>
			<button type="button" class="underline" onclick={() => act(file.id, false)}>Restore</button>
			<button type="button" class="text-danger underline" onclick={() => act(file.id, true)}>Purge</button>
		</li>
	{/each}
</ul>
{#if deleted.hasNextPage}
	<button type="button" class="btn" onclick={() => deleted.fetchNextPage()}>Show more</button>
{/if}
