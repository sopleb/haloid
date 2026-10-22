<script lang="ts">
	import type { FileView } from '$lib/api/types';

	/** Drag to reorder, or use the buttons: both work from the keyboard too. */
	let {
		files,
		onsave,
		oncancel
	}: { files: FileView[]; onsave: (ids: string[]) => Promise<void>; oncancel: () => void } = $props();

	// Starts from the files given; moves overwrite it locally until saved.
	let order = $derived([...files]);
	let dragging = $state<number | null>(null);
	let saving = $state(false);
	let announcement = $state('');

	function move(from: number, to: number) {
		if (to < 0 || to >= order.length || from === to) return;
		const next = [...order];
		const [item] = next.splice(from, 1);
		next.splice(to, 0, item);
		order = next;
		announcement = `${item.title ?? 'File'} moved to position ${to + 1} of ${order.length}.`;
	}

	async function save() {
		saving = true;
		try {
			await onsave(order.map((f) => f.id));
		} finally {
			saving = false;
		}
	}
</script>

<section aria-label="Arrange the album" class="grid gap-3">
	<p class="text-sm muted">Drag the pictures, or use the arrows. Files not loaded yet keep their place after these.</p>
	<p class="sr-only" aria-live="polite">{announcement}</p>
	<ol class="grid grid-cols-2 gap-3 sm:grid-cols-4 lg:grid-cols-6" data-testid="arrange-list">
		{#each order as file, i (file.id)}
			<li
				class="grid gap-1 rounded-card border p-1 {dragging === i ? 'border-accent opacity-60' : 'border-line'}"
				draggable="true"
				ondragstart={(e) => {
					dragging = i;
					e.dataTransfer?.setData('text/plain', String(i));
				}}
				ondragover={(e) => e.preventDefault()}
				ondrop={(e) => {
					e.preventDefault();
					if (dragging !== null) move(dragging, i);
					dragging = null;
				}}
				ondragend={() => (dragging = null)}
				data-testid="arrange-item"
				data-id={file.id}
			>
				{#if file.urls.thumb}
					<img src={file.urls.thumb} alt={file.title ?? ''} class="aspect-square w-full rounded object-cover" />
				{/if}
				<div class="flex justify-between text-sm">
					<button type="button" aria-label="Move {file.title ?? 'file'} earlier" disabled={i === 0} onclick={() => move(i, i - 1)}>←</button>
					<span class="muted">{i + 1}</span>
					<button
						type="button"
						aria-label="Move {file.title ?? 'file'} later"
						disabled={i === order.length - 1}
						onclick={() => move(i, i + 1)}>→</button
					>
				</div>
			</li>
		{/each}
	</ol>
	<div class="flex gap-3">
		<button type="button" class="btn btn-accent" disabled={saving} onclick={save}>
			{saving ? 'Saving…' : 'Save order'}
		</button>
		<button type="button" class="btn" onclick={oncancel}>Cancel</button>
	</div>
</section>
