<script lang="ts">
	import { goto } from '$app/navigation';
	import { resolve } from '$app/paths';
	import { ApiError } from '$lib/api/client';
	import { configQuery, registerMutation } from '$lib/api/queries';
	import Altcha from '$lib/components/Altcha.svelte';
	import Field from '$lib/components/Field.svelte';

	let username = $state('');
	let email = $state('');
	let password = $state('');
	let altcha = $state<string | null>(null);
	let widget = $state<Altcha>();
	const config = configQuery();
	const mutation = registerMutation();
	const fieldErrors = $derived(
		mutation.error instanceof ApiError ? (mutation.error.problem.errors ?? {}) : {}
	);

	function submit(event: SubmitEvent) {
		event.preventDefault();
		if (!altcha) return;
		mutation.mutate(
			{ username, email: email || undefined, password, altcha },
			{
				// Registering does not sign you in: that takes a fresh proof-of-work.
				onSuccess: () => goto(resolve('/login'), { state: { registered: true } }),
				onError: () => widget?.reset()
			}
		);
	}
</script>

<svelte:head><title>Create an account</title></svelte:head>

{#if config.data && !config.data.registration_open}
	<p class="text-center">Registration is closed here.</p>
{:else}
	<form class="mx-auto grid max-w-sm gap-4" onsubmit={submit}>
		<h1 class="display text-5xl md:text-6xl">Create an account</h1>
		<Field
			label="Username"
			name="username"
			autocomplete="username"
			required
			bind:value={username}
			errors={fieldErrors.username}
		/>
		<Field
			label="Email (optional, for password resets)"
			name="email"
			type="email"
			autocomplete="email"
			bind:value={email}
			errors={fieldErrors.email}
		/>
		<Field
			label="Password"
			name="password"
			type="password"
			autocomplete="new-password"
			required
			minlength={8}
			bind:value={password}
			errors={fieldErrors.password}
		/>
		<Altcha bind:this={widget} bind:payload={altcha} />
		{#if mutation.error && Object.keys(fieldErrors).length === 0}
			<p class="text-danger" role="alert">
				{mutation.error instanceof ApiError ? mutation.error.message : 'Registration failed.'}
			</p>
		{/if}
		<button
			type="submit"
			class="btn btn-accent"
			disabled={!altcha || mutation.isPending}
		>
			{mutation.isPending ? 'Creating…' : 'Create account'}
		</button>
	</form>
{/if}
