<script lang="ts">
	import { goto } from '$app/navigation';
	import { resolve } from '$app/paths';
	import { page } from '$app/state';
	import { ApiError } from '$lib/api/client';
	import { loginMutation } from '$lib/api/queries';
	import Altcha from '$lib/components/Altcha.svelte';
	import Field from '$lib/components/Field.svelte';

	let login = $state('');
	let password = $state('');
	let altcha = $state<string | null>(null);
	let widget = $state<Altcha>();
	const mutation = loginMutation();
	const registered = $derived(Boolean((page.state as { registered?: boolean }).registered));

	function submit(event: SubmitEvent) {
		event.preventDefault();
		if (!altcha) return;
		mutation.mutate(
			{ login, password, altcha },
			{
				onSuccess: () => goto(resolve('/')),
				onError: () => widget?.reset()
			}
		);
	}
</script>

<svelte:head><title>Sign in</title></svelte:head>

<form class="mx-auto grid max-w-sm gap-4" onsubmit={submit}>
	<h1 class="display text-5xl md:text-6xl">Sign in</h1>
	{#if registered}
		<p role="status">Your account is ready. Sign in to start.</p>
	{/if}
	<Field label="Username or email" name="login" autocomplete="username" required bind:value={login} />
	<Field
		label="Password"
		name="password"
		type="password"
		autocomplete="current-password"
		required
		bind:value={password}
	/>
	<Altcha bind:this={widget} bind:payload={altcha} />
	{#if mutation.error}
		<p class="text-danger" role="alert">
			{mutation.error instanceof ApiError ? mutation.error.message : 'Signing in failed.'}
		</p>
	{/if}
	<button
		type="submit"
		class="btn btn-accent"
		disabled={!altcha || mutation.isPending}
	>
		{mutation.isPending ? 'Signing in…' : 'Sign in'}
	</button>
	<p class="text-sm muted">
		New here? <a class="underline" href={resolve('/register')}>Create an account</a>
	</p>
</form>
