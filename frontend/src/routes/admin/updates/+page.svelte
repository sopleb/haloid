<script lang="ts">
	import { createQuery, useQueryClient } from '@tanstack/svelte-query';
	import { ApiError, admin } from '$lib/api/client';
	import type { UpdateView } from '$lib/api/types';

	const client = useQueryClient();
	// While an install runs, and through the restart that follows, keep
	// asking; a failed request then only means the server is coming back.
	let installing = $state<string | null>(null);
	const updates = createQuery(() => ({
		queryKey: ['updates'],
		queryFn: admin.updates,
		refetchInterval: installing ? 2000 : false,
		retry: installing ? true : 1
	}));
	let busy = $state(false);
	let error = $state<string | null>(null);
	let notice = $state<string | null>(null);

	const modes = [
		{ value: 'notify', label: 'Tell me, and let an administrator install' },
		{ value: 'auto', label: 'Install by itself, at a set hour' },
		{ value: 'off', label: 'Do not look for releases' }
	];

	const day = (iso: string) =>
		new Date(iso).toLocaleString(undefined, { dateStyle: 'medium', timeStyle: 'short' });

	// Once the server answers with the version it was installing, it is done.
	$effect(() => {
		const u = updates.data;
		if (!installing || !u) return;
		if (u.current === installing) {
			notice = `Haloid ${installing} is running.`;
			installing = null;
		} else if (u.phase === 'failed') {
			error = u.failure ?? 'The update failed; nothing was changed.';
			installing = null;
		}
	});

	async function check() {
		busy = true;
		error = null;
		notice = null;
		try {
			const u = await admin.checkUpdates();
			client.setQueryData(['updates'], u);
			if (!u.error && !u.available) notice = 'This is the newest release.';
		} catch (e) {
			error = e instanceof ApiError ? e.message : 'Looking for a release failed.';
		} finally {
			busy = false;
		}
	}

	async function install(u: UpdateView) {
		if (!u.available) return;
		const ok = confirm(
			`Install Haloid ${u.available.version} and restart?\n\n` +
				'Back up the database first: the new version may change it, and going back to the old binary does not undo that.'
		);
		if (!ok) return;
		busy = true;
		error = null;
		notice = null;
		try {
			const started = await admin.installUpdate();
			installing = started.version;
		} catch (e) {
			error = e instanceof ApiError ? e.message : 'The update could not start.';
		} finally {
			busy = false;
		}
	}

	async function save(key: string, value: string) {
		error = null;
		try {
			await admin.updateSettings({ [key]: value });
			await client.invalidateQueries({ queryKey: ['updates'] });
		} catch (e) {
			error = e instanceof ApiError ? e.message : 'Saving failed.';
		}
	}

	const working = (phase: string) => ['downloading', 'installing', 'restarting'].includes(phase);
	const phaseText: Record<string, string> = {
		downloading: 'Downloading…',
		installing: 'Checking and putting it in place…',
		restarting: 'Restarting into the new version…'
	};
</script>

<svelte:head><title>Updates</title></svelte:head>

<h1 class="font-display text-3xl font-bold tracking-[-0.035em]">Updates</h1>

{#if updates.data}
	{@const u = updates.data}
	{#if !u.enabled}
		<p class="card p-5" data-testid="updates-disabled">
			This install does not update itself: {u.disabled_reason}.
		</p>
	{:else}
		<section class="grid gap-3 md:grid-cols-2" aria-label="Versions">
			<div class="card grid content-start gap-1 p-5">
				<p class="text-sm muted">Running</p>
				<p class="display text-5xl" data-testid="current-version">{u.current}</p>
				<p class="text-sm muted">
					{u.edition === 'pro' ? 'Pro' : 'Community'} edition{#if u.checked_at}
						· looked {day(u.checked_at)}{/if}
				</p>
			</div>
			<div class="card grid content-start gap-2 p-5" data-testid="release">
				{#if installing || working(u.phase)}
					<p class="text-sm muted">Installing {installing ?? u.available?.version}</p>
					<p class="font-display text-2xl font-bold" role="status">{phaseText[u.phase] ?? 'Restarting…'}</p>
				{:else if u.available}
					<p class="text-sm muted">Available</p>
					<p class="display text-5xl text-accent-soft-ink">{u.available.version}</p>
					<p class="text-sm muted">
						Released {day(u.available.published_at)}{#if u.available.notes_url}
							· <a class="underline" href={u.available.notes_url} rel="external noopener">what changed</a>{/if}
					</p>
					{#if u.installable}
						<div><button class="btn btn-accent" disabled={busy} onclick={() => install(u)}>Install and restart</button></div>
					{:else}
						<p class="text-sm">It cannot install here: {u.not_installable}. Install it by hand; see the deploy guide.</p>
					{/if}
				{:else}
					<p class="text-sm muted">Available</p>
					<p class="font-display text-2xl font-bold">Nothing newer</p>
				{/if}
				<div><button class="btn" disabled={busy || !!installing} onclick={check}>Look now</button></div>
			</div>
		</section>

		{#if u.phase === 'failed' && u.failure && !installing}
			<p class="card p-4 text-sm" role="alert">The last update failed, and nothing was changed: {u.failure}</p>
		{/if}
		{#if u.error}
			<p class="card p-4 text-sm" role="alert">Looking for a release failed: {u.error}</p>
		{/if}

		<section class="card grid max-w-2xl gap-4 p-5" aria-label="When to update">
			<label class="grid gap-1.5">
				<span class="font-display text-xl font-bold tracking-[-0.03em]">When a release comes out</span>
				<select class="field" value={u.mode} onchange={(e) => save('updates.mode', e.currentTarget.value)}>
					{#each modes as mode (mode.value)}<option value={mode.value}>{mode.label}</option>{/each}
				</select>
			</label>
			{#if u.mode === 'auto'}
				<label class="grid gap-1.5">
					<span>At (UTC)</span>
					<select class="field w-auto" value={String(u.hour)} onchange={(e) => save('updates.hour', e.currentTarget.value)}>
						{#each Array.from({ length: 24 }, (_, h) => h) as h (h)}
							<option value={String(h)}>{String(h).padStart(2, '0')}:00</option>
						{/each}
					</select>
				</label>
			{/if}
			<p class="text-sm muted">
				Releases are signed, and checked before anything is replaced. The old binary is kept, and
				<code>haloid-server rollback</code> puts it back; the database is not rolled back, so keep backups.
			</p>
		</section>
	{/if}
	{#if error}<p class="text-danger" role="alert">{error}</p>{/if}
	{#if notice}<p role="status">{notice}</p>{/if}
{:else if updates.isError && !installing}
	<p class="text-danger" role="alert">{updates.error.message}</p>
{/if}
