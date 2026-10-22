<script lang="ts">
	import { onMount } from 'svelte';
	import { uploads } from '$lib/state/uploads.svelte';
	import type { Pending } from '$lib/upload/resume';
	import { size } from '$lib/upload/preflight';

	let pending = $state<Pending[]>([]);
	let notice = $state<string | null>(null);

	async function refresh() {
		pending = await uploads.interrupted();
	}
	onMount(refresh);

	async function reselect(files: FileList | null) {
		if (!files?.length) return;
		const matched = await uploads.resume(files);
		notice =
			matched === files.length
				? null
				: `${files.length - matched} of those did not match an interrupted upload: they need to be the same files, unchanged.`;
		await refresh();
	}

	async function discard(session: string) {
		await uploads.discard(session);
		await refresh();
	}
</script>

{#if pending.length}
	<section class="card mb-6 p-5" aria-labelledby="resume-title" data-testid="resume-prompt">
		<h2 id="resume-title" class="font-medium">Interrupted uploads</h2>
		<p class="text-sm muted">
			Choose the same files again and they carry on from where they stopped.
		</p>
		<ul class="my-3 grid gap-1 text-sm">
			{#each pending as item (item.session)}
				<li class="flex items-center gap-3">
					<span class="min-w-0 flex-1 truncate">{item.name}</span>
					<span class="muted">{size(item.size)}</span>
					<button type="button" class="muted underline" onclick={() => discard(item.session)}>
						Forget
					</button>
				</li>
			{/each}
		</ul>
		<label class="btn btn-small cursor-pointer">
			Re-select files
			<input
				class="sr-only"
				type="file"
				multiple
				data-testid="resume-input"
				onchange={(e) => {
					reselect(e.currentTarget.files);
					e.currentTarget.value = '';
				}}
			/>
		</label>
		{#if notice}<p class="mt-2 text-sm" role="status">{notice}</p>{/if}
	</section>
{/if}
