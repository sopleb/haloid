<script lang="ts">
	import type { AlbumView } from '$lib/api/types';

	export interface AlbumFields {
		name: string;
		description: string | null;
		privacy: 'public' | 'unlisted' | 'private' | 'password';
		password?: string;
	}

	let {
		album,
		submitLabel,
		onsubmit
	}: {
		album?: AlbumView;
		submitLabel: string;
		onsubmit: (fields: AlbumFields) => Promise<void>;
	} = $props();

	let name = $state('');
	let description = $state('');
	let privacy = $state<AlbumFields['privacy']>('public');
	let password = $state('');
	$effect.pre(() => {
		name = album?.name ?? '';
		description = album?.description ?? '';
		privacy = (album?.privacy as AlbumFields['privacy']) ?? 'public';
	});
	let busy = $state(false);

	async function submit(event: SubmitEvent) {
		event.preventDefault();
		busy = true;
		try {
			await onsubmit({
				name,
				description: description.trim() || null,
				privacy,
				...(password ? { password } : {})
			});
			password = '';
		} finally {
			busy = false;
		}
	}
</script>

<form class="grid gap-3" onsubmit={submit}>
	<label class="grid gap-1 text-sm">
		Name
		<input bind:value={name} required maxlength={190} class="field" />
	</label>
	<label class="grid gap-1 text-sm">
		Description
		<textarea bind:value={description} rows="2" class="field"
		></textarea>
	</label>
	<label class="grid gap-1 text-sm">
		Who can see it
		<select bind:value={privacy} class="field">
			<option value="public">Everyone, and listed</option>
			<option value="unlisted">Anyone with the link</option>
			<option value="private">Only me</option>
			<option value="password">Anyone with a password</option>
		</select>
	</label>
	{#if privacy === 'password'}
		<label class="grid gap-1 text-sm">
			{album?.privacy === 'password' ? 'New password (leave empty to keep it)' : 'Password'}
			<input
				type="password"
				bind:value={password}
				autocomplete="new-password"
				required={album?.privacy !== 'password'}
				class="field"
			/>
		</label>
	{/if}
	<div>
		<button
			type="submit"
			class="btn btn-accent"
			disabled={busy}
		>
			{submitLabel}
		</button>
	</div>
</form>
