<script lang="ts">
	import { resolve } from '$app/paths';
	import { ApiError, uploadFromUrl } from '$lib/api/client';
	import { configQuery } from '$lib/api/queries';
	import type { FileView } from '$lib/api/types';
	import EmbedPanel from '$lib/components/EmbedPanel.svelte';
	import Icon from '$lib/components/Icon.svelte';
	import ResumePrompt from '$lib/components/ResumePrompt.svelte';
	import UploadDock from '$lib/components/UploadDock.svelte';
	import { uploads } from '$lib/state/uploads.svelte';
	import { preflight, size } from '$lib/upload/preflight';

	const config = configQuery();
	let dragging = $state(false);

	// Checked before a byte leaves, against this caller's own limits.
	$effect(() => {
		const current = config.data;
		if (current) uploads.setPreflight((file) => preflight(file, current));
	});

	let link = $state('');
	let fetching = $state(false);
	let fetched = $state<FileView[]>([]);
	let linkError = $state<string | null>(null);
	const done = $derived([
		...(uploads.items.flatMap((item) => (item.result ? [item.result] : [])) as FileView[]),
		...fetched
	]);

	async function fromUrl(event: SubmitEvent) {
		event.preventDefault();
		fetching = true;
		linkError = null;
		try {
			const { file } = await uploadFromUrl(link.trim());
			fetched = [...fetched, file];
			link = '';
		} catch (e) {
			linkError = e instanceof ApiError ? e.message : 'That URL could not be uploaded.';
		} finally {
			fetching = false;
		}
	}

	// Audio is Pro: only offered when this instance takes it.
	const audio = $derived(
		config.data?.uploads.extensions.some((e) => ['mp3', 'm4a', 'wav'].includes(e)) ?? true
	);

	function send(files: FileList | null | undefined) {
		if (files?.length) uploads.add(files);
	}
</script>

<svelte:head><title>{config.data?.site_name ?? 'Haloid'}</title></svelte:head>

{#if config.data && !config.data.uploads.allowed}
	<section class="grid gap-6 py-10 md:py-20" data-testid="sign-in-to-upload">
		<h1 class="display text-[clamp(4rem,16vw,10rem)]">Sign in<span class="text-accent">.</span></h1>
		<p class="max-w-md text-lg muted">Uploading here needs an account.</p>
		<div><a href={resolve('/login')} class="btn btn-accent">Sign in</a></div>
	</section>
{:else}
	<ResumePrompt />
	<div class="grid items-end gap-6 md:grid-cols-[1.1fr_1fr] md:gap-10">
		<h1 class="display text-[clamp(4.75rem,21vw,11rem)] md:pb-2">Drop<br />it here<span class="text-accent">.</span></h1>
		<section
			class="relative grid min-h-44 rounded-card p-5 transition-colors md:min-h-72 md:p-7
				{dragging ? 'bg-ink text-surface' : 'bg-accent text-accent-ink'}"
			aria-label="Upload"
			ondragover={(e) => {
				e.preventDefault();
				dragging = true;
			}}
			ondragleave={() => (dragging = false)}
			ondrop={(e) => {
				e.preventDefault();
				dragging = false;
				send(e.dataTransfer?.files);
			}}
		>
			<label class="grid cursor-pointer grid-cols-[1fr_auto] items-end gap-4 rounded-[1rem] focus-within:outline-2 focus-within:outline-offset-4 focus-within:outline-current">
				<span>
					<span class="block font-display text-[2.15rem] leading-[0.95] font-extrabold tracking-[-0.035em] md:text-5xl">
						{dragging ? 'Let go' : 'Choose files'}
					</span>
					<span class="mt-2 block text-[0.9375rem] font-medium">
						{audio ? 'Pictures, video and sound' : 'Pictures and video'}{config.data
							? `, up to ${size(config.data.uploads.max_size)} each`
							: ''}.
					</span>
				</span>
				<span class="grid size-16 place-items-center rounded-full bg-accent-ink text-accent md:size-20">
					<Icon name="plus" size={30} />
				</span>
				<input
					class="sr-only"
					type="file"
					multiple
					accept={audio ? 'image/*,video/*,audio/*' : 'image/*,video/*'}
					data-testid="file-input"
					onchange={(e) => {
						send(e.currentTarget.files);
						e.currentTarget.value = '';
					}}
				/>
			</label>
		</section>
	</div>

	{#if config.data?.uploads.url_sources}
		<form class="mt-3 flex items-center gap-2 rounded-card bg-surface-raised p-2 pl-4 md:ml-auto md:w-[calc(50%-1.25rem)]" onsubmit={fromUrl} data-testid="url-form">
			<Icon name="link" class="muted" />
			<label class="min-w-0 flex-1">
				<span class="sr-only">Or from a URL</span>
				<input
					type="url"
					required
					placeholder="Or paste a link to fetch"
					bind:value={link}
					class="w-full bg-transparent py-2 placeholder:text-ink-muted focus:outline-none"
				/>
			</label>
			<button class="btn btn-small" disabled={fetching}>{fetching ? 'Fetching…' : 'Fetch'}</button>
		</form>
		{#if linkError}<p class="mt-2 text-danger" role="alert">{linkError}</p>{/if}
		{#each fetched as file (file.id)}
			<p class="mt-2 text-sm">
				Uploaded: <a class="font-medium underline" href={resolve('/f/[id]/[[slug]]', { id: file.id })}>{file.title ?? file.id}</a>
			</p>
		{/each}
	{/if}

	<UploadDock />

	{#if done.length}
		<div class="mt-6"><EmbedPanel files={done} /></div>
	{/if}
{/if}
