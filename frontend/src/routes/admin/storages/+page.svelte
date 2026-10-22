<script lang="ts">
	import { createQuery, useQueryClient } from '@tanstack/svelte-query';
	import { ApiError, admin } from '$lib/api/client';
	import type { StorageBody, StorageTestView } from '$lib/api/types';
	import { resolve } from '$app/paths';
	import { size } from '$lib/upload/preflight';

	const client = useQueryClient();
	const storages = createQuery(() => ({ queryKey: ['storages'], queryFn: admin.storages }));
	// Which kinds this build has: S3 is in the Pro build only.
	const licence = createQuery(() => ({ queryKey: ['licence'], queryFn: admin.licence }));
	const external = $derived(licence.data?.storage_kinds.includes('s3') && licence.data.pro);
	let results = $state<Record<number, StorageTestView>>({});
	let testing = $state<number | null>(null);
	let error = $state<string | null>(null);
	let notice = $state<string | null>(null);

	let name = $state('');
	let kind = $state('local');
	let urlPrefix = $state('');
	let config = $state('{}');

	async function test(id: number) {
		testing = id;
		try {
			results[id] = await admin.testStorage(id);
		} catch (e) {
			error = e instanceof ApiError ? e.message : 'The test could not run.';
		} finally {
			testing = null;
		}
	}

	async function create(event: SubmitEvent) {
		event.preventDefault();
		let parsed: Record<string, unknown>;
		try {
			parsed = JSON.parse(config);
		} catch {
			error = 'The configuration is not valid JSON.';
			return;
		}
		try {
			await admin.createStorage({
				name,
				kind,
				url_prefix: urlPrefix,
				// The schema types the kind-specific object loosely.
				config: parsed as StorageBody['config'],
				is_active: true,
				priority: 0
			});
			name = urlPrefix = '';
			config = '{}';
			error = null;
			notice = 'Added, and in use now.';
			await client.invalidateQueries({ queryKey: ['storages'] });
		} catch (e) {
			error = e instanceof ApiError ? e.message : 'Adding the storage failed.';
		}
	}

	async function retire(id: number) {
		if (!confirm('Retire this storage? It takes no new uploads and keeps serving what it has.')) return;
		await admin.retireStorage(id);
		await client.invalidateQueries({ queryKey: ['storages'] });
	}
</script>

<svelte:head><title>Storages</title></svelte:head>

<h1 class="font-display text-3xl font-bold tracking-[-0.035em]">Storages</h1>
{#if error}<p class="text-danger" role="alert">{error}</p>{/if}
{#if notice}<p role="status">{notice}</p>{/if}
<ul class="grid gap-4">
	{#each storages.data ?? [] as storage (storage.id)}
		<li class="card grid gap-2 p-5" data-testid="storage">
			<div class="flex flex-wrap items-center gap-3">
				<span class="font-medium">{storage.name}</span>
				<span class="text-sm muted">{storage.kind} · {storage.url_prefix}</span>
				<span class="text-sm muted">{storage.file_count} files · {size(storage.used_bytes)}{storage.capacity_bytes ? ` of ${size(storage.capacity_bytes)}` : ''}</span>
				{#if storage.retired_at}<span class="text-sm muted">retired</span>{/if}
				<span class="ml-auto flex gap-2">
					<button type="button" class="btn btn-small" disabled={testing === storage.id} onclick={() => test(storage.id)}>
						{testing === storage.id ? 'Testing…' : 'Test'}
					</button>
					{#if !storage.retired_at}
						<button type="button" class="btn btn-small" onclick={() => retire(storage.id)}>Retire</button>
					{/if}
				</span>
			</div>
			{#if results[storage.id]}
				<ul class="grid gap-1 text-sm" data-testid="storage-checks">
					{#each results[storage.id].checks as check (check.name)}
						<li class={check.ok ? '' : 'text-danger'}>{check.ok ? '✓' : '✗'} {check.name}: {check.detail}</li>
					{/each}
				</ul>
			{/if}
		</li>
	{/each}
</ul>

<form class="card grid max-w-lg gap-3 p-5" onsubmit={create}>
	<h2 class="font-medium">Add a storage</h2>
	<label class="grid gap-1 text-sm">Name <input required bind:value={name} class="field min-h-10 py-1" /></label>
	<label class="grid gap-1 text-sm">Kind
		<select bind:value={kind} class="field min-h-10 py-1">
			<option value="local">Local folder</option>
			{#if external}<option value="s3">S3-compatible</option>{/if}
		</select>
	</label>
	{#if licence.data && !external}
		<p class="text-sm muted" data-testid="storage-pro-note">
			S3 and other external storage come with <a class="font-medium text-ink underline" href={resolve('/admin/licence')}>Haloid Pro</a>.
		</p>
	{/if}
	<label class="grid gap-1 text-sm">Public URL prefix <input required bind:value={urlPrefix} placeholder="https://cdn.example.com/files" class="field min-h-10 py-1" /></label>
	<label class="grid gap-1 text-sm">Configuration (JSON)
		<textarea rows="4" bind:value={config} class="field min-h-10 py-1"></textarea>
	</label>
	<p class="text-xs muted">
		Credentials never go here: an S3 storage reads them from HALOID_STORAGE_&lt;NAME&gt;_ACCESS_KEY and _SECRET_KEY.
	</p>
	<div><button class="btn btn-accent">Add storage</button></div>
</form>
