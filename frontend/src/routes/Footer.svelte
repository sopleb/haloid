<script lang="ts">
	import { configQuery } from '$lib/api/queries';
	import { prefs } from '$lib/state/prefs.svelte';
	import { parseChoice, themes } from '$lib/theme/themes';

	const config = configQuery();

	// The administrator's default arrives with the config; the visitor's own
	// choice, if any, wins over it.
	$effect(() => {
		const site = config.data?.theme;
		if (site && site !== prefs.siteTheme) prefs.setSiteTheme(site);
	});
	$effect(() => {
		// Read both so a change to either re-applies.
		void prefs.theme;
		void prefs.siteTheme;
		prefs.apply();
		if (prefs.theme !== 'device') return;
		const query = matchMedia('(prefers-color-scheme: light)');
		const follow = () => prefs.apply();
		query.addEventListener('change', follow);
		return () => query.removeEventListener('change', follow);
	});
	const id = $props.id();
</script>

<footer class="mx-auto flex w-full max-w-6xl flex-wrap items-center gap-x-6 gap-y-3 px-4 pt-6 pb-28 text-sm md:px-6 md:pb-8">
	<label for={id} class="flex items-center gap-2 muted">
		Theme
		<select
			{id}
			class="field min-h-9 w-auto py-1 text-sm"
			value={prefs.theme}
			onchange={(e) => prefs.setTheme(parseChoice(e.currentTarget.value))}
		>
			<option value="site">Site default</option>
			{#each themes as theme (theme.id)}<option value={theme.id}>{theme.name}</option>{/each}
			<option value="device">Match my device</option>
		</select>
	</label>
	<!-- The branding a licence removes; until the config says, it shows. -->
	{#if config.data?.branding ?? true}
		<p class="ml-auto muted" data-testid="branding">
			Powered by <a class="font-medium text-ink hover:underline" href="https://git.plb.so/sopleb/haloid">Haloid</a>
		</p>
	{/if}
</footer>
