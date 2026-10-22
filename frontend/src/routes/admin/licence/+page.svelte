<script lang="ts">
	import { createQuery, useQueryClient } from '@tanstack/svelte-query';
	import { ApiError, admin } from '$lib/api/client';
	import { keys } from '$lib/api/queries';
	import CopyButton from '$lib/components/CopyButton.svelte';

	const client = useQueryClient();
	const licence = createQuery(() => ({ queryKey: ['licence'], queryFn: admin.licence }));
	let key = $state('');
	let error = $state<string | null>(null);
	let saving = $state(false);

	const labels: Record<string, string> = {
		none: 'Community',
		active: 'Pro',
		lapsed: 'Lapsed',
		revoked: 'Revoked'
	};
	const day = (iso: string) =>
		new Date(iso).toLocaleDateString(undefined, { day: 'numeric', month: 'long', year: 'numeric' });

	async function refresh(saved?: Awaited<ReturnType<typeof admin.licence>>) {
		if (saved) client.setQueryData(['licence'], saved);
		else await client.invalidateQueries({ queryKey: ['licence'] });
		// Branding and the accepted formats follow the licence.
		await client.invalidateQueries({ queryKey: keys.config });
	}

	async function install(event: SubmitEvent) {
		event.preventDefault();
		saving = true;
		error = null;
		try {
			await refresh(await admin.installLicence(key.trim()));
			key = '';
		} catch (e) {
			error = e instanceof ApiError ? e.message : 'Installing the key failed.';
		} finally {
			saving = false;
		}
	}

	async function remove() {
		if (!confirm('Remove the licence key? This instance goes back to the Community edition.')) return;
		try {
			await admin.removeLicence();
			await refresh();
		} catch (e) {
			error = e instanceof ApiError ? e.message : 'Removing the key failed.';
		}
	}
</script>

<svelte:head><title>Licence</title></svelte:head>

<h1 class="font-display text-3xl font-bold tracking-[-0.035em]">Licence</h1>

{#if licence.data}
	{@const l = licence.data}
	<section class="grid gap-3 md:grid-cols-2" aria-label="Status">
		<div class="card grid content-start gap-1 p-5" data-testid="licence-state">
			<p class="text-sm muted">Edition</p>
			<p class="display text-5xl {l.state === 'active' ? 'text-accent-soft-ink' : ''}">{labels[l.state]}</p>
			{#if l.licence}
				<p class="mt-2">
					{l.licence.kind === 'trial' ? 'Trial' : l.licence.kind === 'perpetual' ? 'Perpetual licence' : 'Licence'}
					for <span class="font-medium">{l.licence.licensee}</span>
				</p>
				<p class="text-sm muted">
					{#if l.licence.expires_at}
						{l.state === 'lapsed' ? 'Lapsed on' : 'Runs until'} {day(l.licence.expires_at)}
					{:else}
						Never expires
					{/if}
					· key {l.licence.id}
				</p>
			{/if}
			{#if l.state === 'revoked'}
				<p class="mt-2 text-sm">
					The licence service has withdrawn this key. Uploads and storage keep working; the branding is back and
					new Pro use (audio, external storage) needs a valid key.
				</p>
			{/if}
			{#if l.state === 'lapsed'}
				<p class="mt-2 text-sm">
					{l.pro
						? 'Everything keeps working; the "Powered by Haloid" line is back until the licence is renewed.'
						: 'The trial has ended: the branding is back, and audio uploads and new external storage need a licence. Storage already set up keeps working.'}
				</p>
			{/if}
		</div>
		<div class="card grid content-start gap-2 p-5">
			<p class="text-sm muted">Instance id</p>
			<p class="font-display text-2xl font-bold break-all" data-testid="instance-id">{l.instance_id}</p>
			<p class="text-sm muted">A key is issued for one instance: quote this id when buying one or asking for a trial.</p>
			<div><CopyButton text={l.instance_id} label="Copy id" /></div>
		</div>
	</section>

	{#if l.unavailable_storages.length}
		<p class="card p-4 text-sm" role="alert">
			This build cannot load {l.unavailable_storages.join(', ')}: external storage needs the Haloid Pro build. Files
			on {l.unavailable_storages.length === 1 ? 'it' : 'them'} are not served until it runs.
		</p>
	{/if}

	{#if l.from_environment}
		<p class="text-sm muted">The key is set by HALOID_LICENCE_KEY, so it is changed there rather than here.</p>
	{:else}
		<form class="card grid max-w-2xl gap-3 p-5" onsubmit={install}>
			<label class="grid gap-1.5">
				<span class="font-display text-xl font-bold tracking-[-0.03em]">{l.licence ? 'Replace the key' : 'Install a key'}</span>
				<textarea
					required
					rows="3"
					spellcheck="false"
					placeholder="HALOID1.…"
					bind:value={key}
					class="field break-all"
					data-testid="licence-key"
				></textarea>
			</label>
			{#if error}<p class="text-danger" role="alert">{error}</p>{/if}
			<div class="flex flex-wrap gap-2">
				<button class="btn btn-accent" disabled={saving}>{saving ? 'Checking…' : 'Install key'}</button>
				{#if l.licence}<button type="button" class="btn btn-danger" onclick={remove}>Remove key</button>{/if}
			</div>
			<p class="text-sm muted">
				Keys are checked here, offline. Moving a key to another instance goes through support, who deactivate it and
				issue one for the new id.
			</p>
		</form>
	{/if}
{:else if licence.isError}
	<p class="text-danger" role="alert">{licence.error.message}</p>
{/if}
