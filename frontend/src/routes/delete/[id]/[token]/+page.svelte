<script lang="ts">
	import { resolve } from '$app/paths';
	import { page } from '$app/state';
	import { ApiError, deleteFile } from '$lib/api/client';

	let phase = $state<'confirm' | 'deleting' | 'deleted' | 'failed'>('confirm');
	let error = $state('');

	async function confirm() {
		phase = 'deleting';
		try {
			await deleteFile(page.params.id ?? '', page.params.token ?? '');
			phase = 'deleted';
		} catch (e) {
			phase = 'failed';
			error =
				e instanceof ApiError && e.problem.status === 404
					? 'This link does not delete anything: the file is gone already, or the link is wrong.'
					: 'Deleting failed. Try again in a moment.';
		}
	}
</script>

<svelte:head><title>Delete an upload</title><meta name="robots" content="noindex" /></svelte:head>

<section class="mx-auto grid max-w-md gap-4 text-center">
	<h1 class="display text-5xl md:text-6xl">Delete an upload</h1>
	{#if phase === 'deleted'}
		<p role="status">Deleted. The file no longer loads anywhere it was shared.</p>
		<p><a class="underline" href={resolve('/')}>Upload something else</a></p>
	{:else if phase === 'failed'}
		<p class="text-danger" role="alert">{error}</p>
	{:else}
		<p>This removes the upload for everyone who has its link. It cannot be undone from here.</p>
		<div>
			<button
				type="button"
				class="btn bg-danger text-surface hover:bg-danger"
				disabled={phase === 'deleting'}
				onclick={confirm}
			>
				{phase === 'deleting' ? 'Deleting…' : 'Delete it'}
			</button>
		</div>
	{/if}
</section>
