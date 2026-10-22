<script lang="ts">
	import { resolve } from '$app/paths';
	import { createInfiniteQuery, useQueryClient } from '@tanstack/svelte-query';
	import { ApiError, admin } from '$lib/api/client';
	import type { QueuePage } from '$lib/api/types';

	const client = useQueryClient();
	const queue = createInfiniteQuery(() => ({
		queryKey: ['moderation'],
		queryFn: ({ pageParam }) => admin.queue(pageParam),
		initialPageParam: null as string | null,
		getNextPageParam: (last: QueuePage) => last.next ?? undefined
	}));
	const items = $derived(queue.data?.pages.flatMap((p) => p.items) ?? []);
	let error = $state<string | null>(null);

	async function decide(id: string, approve: boolean) {
		try {
			if (approve) await admin.approve(id);
			else await admin.deny(id, prompt('A note for the audit log (optional)') ?? undefined);
			error = null;
			await client.invalidateQueries({ queryKey: ['moderation'] });
		} catch (e) {
			error = e instanceof ApiError ? e.message : 'That did not work.';
		}
	}
</script>

<svelte:head><title>Moderation</title></svelte:head>

<h1 class="font-display text-3xl font-bold tracking-[-0.035em]">Waiting for review</h1>
{#if error}<p class="text-danger" role="alert">{error}</p>{/if}
{#if queue.data && items.length === 0}
	<p class="muted">Nothing is waiting.</p>
{/if}
<ul class="grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-3" data-testid="moderation-queue">
	{#each items as item (item.file.id)}
		<li class="card grid gap-2 p-4" data-testid="queued-file">
			<a href={resolve('/f/[id]/[[slug]]', { id: item.file.id })} class="block aspect-video overflow-hidden rounded bg-surface-raised">
				{#if item.file.urls.thumb}
					<img src={item.file.urls.thumb} alt={item.file.title ?? ''} class="h-full w-full object-contain" />
				{/if}
			</a>
			<p class="text-sm muted">
				{item.file.kind} · queued {new Date(item.queued_at).toLocaleString()}{item.reason ? ` · ${item.reason}` : ''}
			</p>
			<div class="flex gap-2">
				<button type="button" class="btn btn-accent btn-small" onclick={() => decide(item.file.id, true)}>Approve</button>
				<button type="button" class="btn btn-small btn-danger" onclick={() => decide(item.file.id, false)}>Deny</button>
			</div>
		</li>
	{/each}
</ul>
{#if queue.hasNextPage}
	<button type="button" class="btn" onclick={() => queue.fetchNextPage()}>Show more</button>
{/if}
