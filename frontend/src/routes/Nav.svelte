<script lang="ts">
	import { goto } from '$app/navigation';
	import { resolve } from '$app/paths';
	import { page } from '$app/state';
	import { configQuery, logoutMutation, meQuery } from '$lib/api/queries';
	import Icon from '$lib/components/Icon.svelte';

	const me = meQuery();
	const config = configQuery();
	const logout = logoutMutation();
	const staff = $derived(me.data?.role === 'admin' || me.data?.role === 'manager');
	const at = (path: string) => page.url.pathname === path || page.url.pathname.startsWith(`${path}/`);
	const mine = $derived(me.data ? resolve('/u/[username]', { username: me.data.username }) : null);

	// On a phone the tab bar's Upload button is the way in; on the home page
	// it opens the file picker directly.
	function upload() {
		const input = document.querySelector<HTMLInputElement>('[data-testid="file-input"]');
		if (input) input.click();
		else goto(resolve('/'));
	}

	const link = 'inline-flex min-h-11 items-center gap-2 rounded-control px-3 font-medium';
	const quiet = `${link} muted hover:text-ink`;
</script>

<nav class="mx-auto flex max-w-6xl items-center gap-1 px-4 py-3 md:px-6" aria-label="Main">
	<a href={resolve('/')} class="mr-3 flex items-center gap-2 font-display text-2xl font-extrabold tracking-[-0.04em]">
		<span class="size-3 rounded-full bg-accent" aria-hidden="true"></span>
		{config.data?.site_name ?? 'Haloid'}
	</a>
	<a href={resolve('/explore')} class="{at('/explore') ? link : quiet} max-md:hidden">Explore</a>
	{#if mine && me.data}
		<a
			href={resolve('/u/[username]', { username: me.data.username })}
			class="{page.url.pathname === mine ? link : quiet} max-md:hidden"
			data-testid="nav-user"
		>
			{me.data.display_name ?? me.data.username}
		</a>
	{/if}

	<span class="ml-auto flex items-center gap-1">
		{#if me.data}
			{#if staff}
				<a href={resolve('/admin')} class="{at('/admin') ? link : quiet} max-md:px-2.5" aria-label="Admin">
					<Icon name="shield" class="md:hidden" /><span class="max-md:hidden">Admin</span>
				</a>
			{/if}
			<a href={resolve('/account')} class="{at('/account') ? link : quiet} max-md:px-2.5" aria-label="Account">
				<Icon name="user" class="md:hidden" /><span class="max-md:hidden">Account</span>
			</a>
			<button
				type="button"
				class="{quiet} max-md:hidden"
				onclick={() => logout.mutate(undefined, { onSuccess: () => goto(resolve('/')) })}
			>
				Sign out
			</button>
		{:else if me.isSuccess}
			<a href={resolve('/login')} class="{quiet} max-md:hidden">Sign in</a>
			{#if config.data?.registration_open}
				<a href={resolve('/register')} class="btn btn-accent btn-small ml-1">Create account</a>
			{/if}
		{/if}
		{#if page.url.pathname !== '/'}
			<a href={resolve('/')} class="btn btn-accent ml-2 max-md:hidden"><Icon name="up" size={20} />Upload</a>
		{/if}
	</span>
</nav>

<!-- The phone's tab bar: floats above the page, so it is the one thing with a shadow. -->
<nav
	class="fixed inset-x-3 bottom-3 z-20 grid h-[4.25rem] grid-cols-4 items-center rounded-[1.5rem] bg-surface-raised shadow-[0_12px_32px_rgb(0_0_0/0.45)] md:hidden"
	style="margin-bottom: env(safe-area-inset-bottom)"
	aria-label="Tabs"
>
	{#each [{ href: resolve('/'), name: 'Home', icon: 'home', on: page.url.pathname === '/' }, { href: resolve('/explore'), name: 'Explore', icon: 'compass', on: at('/explore') }, mine ? { href: mine, name: 'Mine', icon: 'user', on: page.url.pathname === mine } : { href: resolve('/login'), name: 'Sign in', icon: 'user', on: at('/login') }] as tab (tab.name)}
		<!-- eslint-disable-next-line svelte/no-navigation-without-resolve -- every tab.href above comes from resolve() -->
		<a href={tab.href}
			class="grid justify-items-center gap-0.5 text-xs font-medium {tab.on ? 'text-ink' : 'muted'}"
			aria-current={tab.on ? 'page' : undefined}
		>
			<Icon name={tab.icon as 'home'} />{tab.name}
		</a>
	{/each}
	<button
		type="button"
		class="grid size-[3.25rem] place-items-center justify-self-center rounded-[1.1rem] bg-accent text-accent-ink active:scale-95"
		aria-label="Upload files"
		onclick={upload}
	>
		<Icon name="up" />
	</button>
</nav>
