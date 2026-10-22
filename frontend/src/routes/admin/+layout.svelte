<script lang="ts">
	import { resolve } from '$app/paths';
	import { page } from '$app/state';
	import { createQuery } from '@tanstack/svelte-query';
	import { admin } from '$lib/api/client';
	import { meQuery } from '$lib/api/queries';
	let { children } = $props();

	const me = meQuery();
	const role = $derived(me.data?.role);
	const isAdmin = $derived(role === 'admin');
	const canModerate = $derived(role === 'admin' || role === 'manager');
	// A release waiting, shown on every admin page so it is not missed.
	const updates = createQuery(() => ({
		queryKey: ['updates'],
		queryFn: admin.updates,
		enabled: isAdmin,
		staleTime: 5 * 60_000
	}));
	const waiting = $derived(
		updates.data?.enabled && updates.data.available && updates.data.phase === 'idle'
			? updates.data.available.version
			: null
	);
	const tabs = $derived([
		{ href: resolve('/admin'), label: 'Moderation', show: canModerate },
		{ href: resolve('/admin/settings'), label: 'Settings', show: isAdmin },
		{ href: resolve('/admin/users'), label: 'Users', show: isAdmin },
		{ href: resolve('/admin/storages'), label: 'Storages', show: isAdmin },
		{ href: resolve('/admin/licence'), label: 'Licence', show: isAdmin },
		{ href: resolve('/admin/updates'), label: 'Updates', show: isAdmin },
		{ href: resolve('/admin/deleted'), label: 'Deleted files', show: isAdmin },
		{ href: resolve('/admin/audit'), label: 'Audit log', show: isAdmin }
	]);
</script>

{#if me.isSuccess && !canModerate}
	<p class="py-12 text-center">This area is for moderators and administrators.</p>
{:else if me.data}
	<div class="grid grid-cols-1 gap-6">
		<nav aria-label="Administration" class="-mx-4 flex gap-1.5 overflow-x-auto px-4 pb-1 md:mx-0 md:flex-wrap md:px-0">
			{#each tabs.filter((t) => t.show) as tab (tab.href)}
				<!-- eslint-disable-next-line svelte/no-navigation-without-resolve -- each href was built with resolve() above -->
				<a href={tab.href}
					class="shrink-0 rounded-full px-4 py-2 text-sm font-medium {page.url.pathname === tab.href ? 'bg-ink text-surface' : 'bg-surface-raised muted hover:text-ink'}"
					aria-current={page.url.pathname === tab.href ? 'page' : undefined}>{tab.label}</a
				>
			{/each}
		</nav>
		{#if waiting && page.url.pathname !== resolve('/admin/updates')}
			<p class="card flex flex-wrap items-center gap-x-3 gap-y-1 p-4" data-testid="update-waiting">
				<span>Haloid {waiting} is available.</span>
				<a class="font-medium underline" href={resolve('/admin/updates')}>See the update</a>
			</p>
		{/if}
		{@render children()}
	</div>
{/if}
