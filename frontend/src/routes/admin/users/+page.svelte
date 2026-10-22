<script lang="ts">
	import { resolve } from '$app/paths';
	import { createQuery, useQueryClient } from '@tanstack/svelte-query';
	import { ApiError, admin } from '$lib/api/client';

	const client = useQueryClient();
	let search = $state('');
	const users = createQuery(() => ({ queryKey: ['admin-users', search], queryFn: () => admin.users(search) }));
	let error = $state<string | null>(null);

	async function update(id: string, patch: { role?: string; status?: string }) {
		try {
			await admin.updateUser(id, patch);
			error = null;
		} catch (e) {
			error = e instanceof ApiError ? e.message : 'That did not work.';
		}
		await client.invalidateQueries({ queryKey: ['admin-users'] });
	}
</script>

<svelte:head><title>Users</title></svelte:head>

<div class="flex flex-wrap items-center gap-3">
	<h1 class="font-display text-3xl font-bold tracking-[-0.035em]">Users</h1>
	<input type="search" placeholder="Username or email" aria-label="Search users" bind:value={search} class="field" />
</div>
{#if error}<p class="text-danger" role="alert">{error}</p>{/if}
<div class="-mx-4 overflow-x-auto px-4 md:mx-0 md:px-0">
<table class="w-full text-sm">
	<thead><tr class="text-left muted"><th class="py-2">User</th><th>Files</th><th>Role</th><th>Status</th></tr></thead>
	<tbody>
		{#each users.data?.items ?? [] as user (user.id)}
			<tr class="border-t border-line" data-testid="admin-user">
				<td class="py-2"><a class="underline" href={resolve('/u/[username]', { username: user.username })}>{user.username}</a>
					<span class="block muted">{user.email ?? ''}</span></td>
				<td>{user.file_count}</td>
				<td>
					<select aria-label="Role of {user.username}" value={user.role} onchange={(e) => update(user.id, { role: e.currentTarget.value })} class="field min-h-10 py-1">
						<option value="user">user</option>
						<option value="manager">manager</option>
						<option value="admin">admin</option>
					</select>
				</td>
				<td>
					{#if user.status === 'banned'}
						<button type="button" class="underline" onclick={() => update(user.id, { status: 'valid' })}>Unban</button>
					{:else}
						<button type="button" class="text-danger underline" onclick={() => confirm(`Ban ${user.username}? They are signed out everywhere.`) && update(user.id, { status: 'banned' })}>Ban</button>
					{/if}
				</td>
			</tr>
		{/each}
	</tbody>
</table>
</div>
