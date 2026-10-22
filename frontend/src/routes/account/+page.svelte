<script lang="ts">
	import { resolve } from '$app/paths';
	import { createQuery, useQueryClient } from '@tanstack/svelte-query';
	import * as api from '$lib/api/client';
	import { ApiError } from '$lib/api/client';
	import { keys, meQuery } from '$lib/api/queries';
	import type { CreatedApiKey } from '$lib/api/types';
	import CopyButton from '$lib/components/CopyButton.svelte';
	import Field from '$lib/components/Field.svelte';

	const me = meQuery();
	const client = useQueryClient();
	const apiKeys = createQuery(() => ({
		queryKey: ['api-keys'],
		queryFn: api.apiKeys,
		enabled: Boolean(me.data)
	}));
	const sessions = createQuery(() => ({
		queryKey: ['login-sessions'],
		queryFn: api.loginSessions,
		enabled: Boolean(me.data)
	}));

	const message = (e: unknown, fallback: string) => (e instanceof ApiError ? e.message : fallback);

	// Profile
	let displayName = $state('');
	let email = $state('');
	let isPrivate = $state(false);
	$effect.pre(() => {
		if (!me.data) return;
		displayName = me.data.display_name ?? '';
		email = me.data.email ?? '';
		isPrivate = me.data.is_private;
	});
	let profileNote = $state<string | null>(null);
	async function saveProfile(event: SubmitEvent) {
		event.preventDefault();
		try {
			const saved = await api.updateMe({
				display_name: displayName.trim() || null,
				email: email.trim() || null,
				is_private: isPrivate
			});
			client.setQueryData(keys.me, saved);
			profileNote = 'Saved.';
		} catch (e) {
			profileNote = message(e, 'Saving failed.');
		}
	}

	// Password
	let current = $state('');
	let next = $state('');
	let passwordNote = $state<string | null>(null);
	async function savePassword(event: SubmitEvent) {
		event.preventDefault();
		try {
			await api.changePassword(current, next);
			current = '';
			next = '';
			passwordNote = 'Changed. Every other session has been signed out.';
			await client.invalidateQueries({ queryKey: ['login-sessions'] });
		} catch (e) {
			passwordNote = message(e, 'Changing the password failed.');
		}
	}

	// API keys
	let label = $state('');
	let created = $state<CreatedApiKey | null>(null);
	let keyNote = $state<string | null>(null);
	async function addKey(event: SubmitEvent) {
		event.preventDefault();
		try {
			created = await api.createApiKey(label);
			label = '';
			keyNote = null;
			await client.invalidateQueries({ queryKey: ['api-keys'] });
		} catch (e) {
			keyNote = message(e, 'Creating the key failed.');
		}
	}
	async function revokeKey(id: number) {
		if (!confirm('Revoke this key? Anything using it stops working.')) return;
		await api.revokeApiKey(id);
		if (created?.key.id === id) created = null;
		await client.invalidateQueries({ queryKey: ['api-keys'] });
	}

	async function signOut(id: string) {
		await api.revokeSession(id);
		await client.invalidateQueries({ queryKey: ['login-sessions'] });
	}

	const when = (iso: string | null | undefined) => (iso ? new Date(iso).toLocaleString() : 'never');
</script>

<svelte:head><title>Your account</title></svelte:head>

{#if me.isSuccess && !me.data}
	<p class="py-12 text-center"><a class="underline" href={resolve('/login')}>Sign in</a> to manage your account.</p>
{:else if me.data}
	<div class="mx-auto grid max-w-2xl grid-cols-1 gap-10">
		<h1 class="display text-5xl md:text-6xl">Your account</h1>

		<section aria-labelledby="profile-title" class="grid grid-cols-1 gap-3">
			<h2 id="profile-title" class="font-display text-2xl font-bold tracking-[-0.03em]">Profile</h2>
			<form class="grid max-w-md gap-3" onsubmit={saveProfile}>
				<Field label="Display name" bind:value={displayName} maxlength={190} />
				<Field label="Email" type="email" autocomplete="email" bind:value={email} />
				<label class="flex items-center gap-2 text-sm">
					<input type="checkbox" bind:checked={isPrivate} /> Hide my profile from everyone else
				</label>
				<div><button class="btn btn-accent">Save profile</button></div>
				{#if profileNote}<p role="status">{profileNote}</p>{/if}
			</form>
		</section>

		<section aria-labelledby="password-title" class="grid grid-cols-1 gap-3">
			<h2 id="password-title" class="font-display text-2xl font-bold tracking-[-0.03em]">Password</h2>
			<form class="grid max-w-md gap-3" onsubmit={savePassword}>
				<Field label="Current password" type="password" autocomplete="current-password" required bind:value={current} />
				<Field label="New password" type="password" autocomplete="new-password" required minlength={10} bind:value={next} />
				<div><button class="btn">Change password</button></div>
				{#if passwordNote}<p role="status">{passwordNote}</p>{/if}
			</form>
		</section>

		<section aria-labelledby="keys-title" class="grid grid-cols-1 gap-3">
			<h2 id="keys-title" class="font-display text-2xl font-bold tracking-[-0.03em]">API keys</h2>
			<p class="text-sm muted">
				For ShareX and other upload tools. A key acts as you; keep it like a password.
			</p>
			<form class="flex flex-wrap items-end gap-3" onsubmit={addKey}>
				<Field label="Label" placeholder="ShareX on my laptop" maxlength={190} bind:value={label} />
				<button class="btn">Create key</button>
			</form>
			{#if keyNote}<p class="text-danger" role="alert">{keyNote}</p>{/if}
			{#if created}
				<div class="card grid gap-2 p-4 outline-2 outline-accent" data-testid="new-key">
					<p class="text-sm">This is the only time the whole key is shown.</p>
					<code class="break-all rounded bg-surface-raised p-2 text-sm" data-testid="new-key-secret">{created.secret}</code>
					<div><CopyButton text={created.secret} label="Copy key" /></div>
				</div>
			{/if}
			<ul class="grid grid-cols-1 gap-2" data-testid="key-list">
				{#each apiKeys.data ?? [] as key (key.id)}
					<li class="flex flex-wrap items-center gap-3 rounded-[1.1rem] bg-surface-raised p-3 text-sm">
						<code>{key.prefix}…</code>
						<span class="min-w-0 flex-1 truncate">{key.label || 'Unlabelled'}</span>
						<span class="muted">used {when(key.last_used_at)}</span>
						{#if key.active}
							<button type="button" class="text-danger underline" onclick={() => revokeKey(key.id)}>Revoke</button>
						{:else}
							<span class="muted">revoked</span>
						{/if}
					</li>
				{/each}
			</ul>
		</section>

		<section aria-labelledby="sessions-title" class="grid grid-cols-1 gap-3">
			<h2 id="sessions-title" class="font-display text-2xl font-bold tracking-[-0.03em]">Where you are signed in</h2>
			<ul class="grid grid-cols-1 gap-2">
				{#each sessions.data ?? [] as session (session.id)}
					<li class="flex flex-wrap items-center gap-3 rounded-[1.1rem] bg-surface-raised p-3 text-sm">
						<span class="min-w-0 flex-1 truncate">{session.user_agent ?? 'Unknown browser'}</span>
						<span class="muted">{session.ip ?? ''} · last seen {when(session.last_seen_at)}</span>
						{#if session.current}
							<span class="muted">this one</span>
						{:else}
							<button type="button" class="underline" onclick={() => signOut(session.id)}>Sign out</button>
						{/if}
					</li>
				{/each}
			</ul>
		</section>
	</div>
{/if}
