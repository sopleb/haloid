<script lang="ts">
	import { createQuery } from '@tanstack/svelte-query';
	import { admin } from '$lib/api/client';

	let before = $state<number | undefined>(undefined);
	const log = createQuery(() => ({ queryKey: ['audit', before], queryFn: () => admin.audit(before) }));
	const entries = $derived(log.data ?? []);
</script>

<svelte:head><title>Audit log</title></svelte:head>

<h1 class="font-display text-3xl font-bold tracking-[-0.035em]">Audit log</h1>
<div class="-mx-4 overflow-x-auto px-4 md:mx-0 md:px-0">
<table class="w-full text-sm" data-testid="audit-log">
	<thead><tr class="text-left muted"><th class="py-2">When</th><th>Who</th><th>What</th><th>Target</th><th>Detail</th></tr></thead>
	<tbody>
		{#each entries as entry (entry.id)}
			<tr class="border-t border-line align-top">
				<td class="py-2 pr-3 whitespace-nowrap">{new Date(entry.created_at).toLocaleString()}</td>
				<td class="pr-3">{entry.actor ?? '—'}{entry.ip ? ` (${entry.ip})` : ''}</td>
				<td class="pr-3 font-mono">{entry.action}</td>
				<td class="pr-3">{entry.target ? `${entry.target} ${entry.target_id ?? ''}` : ''}</td>
				<td class="font-mono text-xs break-all">{JSON.stringify(entry.detail)}</td>
			</tr>
		{/each}
	</tbody>
</table>
</div>
{#if entries.length === 100}
	<button type="button" class="btn" onclick={() => (before = entries.at(-1)?.id)}>Older</button>
{/if}
