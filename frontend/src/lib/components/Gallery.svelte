<script lang="ts">
	import { pushState, replaceState } from '$app/navigation';
	import { resolve } from '$app/paths';
	import { page } from '$app/state';
	import { onMount, tick } from 'svelte';
	import type { FileView } from '$lib/api/types';
	import { prefs, reducedMotion } from '$lib/state/prefs.svelte';
	import { WINDOW_FROM, visibleRange } from '$lib/state/window';
	import Icon from './Icon.svelte';
	import Lightbox from './Lightbox.svelte';

	let {
		files,
		selected,
		ontoggle,
		hasMore = false,
		loadMore
	}: {
		files: FileView[];
		/** When given, each file gets a checkbox: the gallery is selectable. */
		selected?: ReadonlySet<string>;
		ontoggle?: (id: string) => void;
		hasMore?: boolean;
		loadMore?: () => void;
	} = $props();

	const still = reducedMotion();
	const thumb = (file: FileView) =>
		(still && file.is_animated ? file.urls.frame : null) ?? file.urls.thumb ?? file.urls.medium;

	// Windowing the uniform grid; masonry is never windowed, a documented
	// trade-off (see design.md).
	let list = $state<HTMLElement>();
	let width = $state(0);
	let scrolledPast = $state(0);
	let viewport = $state(800);
	const columns = $derived(width >= 1024 ? 4 : width >= 640 ? 3 : 2);
	const gap = $derived(width >= 768 ? 8 : 3);
	const rowHeight = $derived(width ? (width - gap * (columns - 1)) / columns + gap : 0);
	const windowed = $derived(prefs.layout === 'grid' && files.length >= WINDOW_FROM && rowHeight > 0);
	const range = $derived(
		windowed
			? visibleRange({ scrolledPast, viewport, rowHeight, columns, count: files.length })
			: { start: 0, end: files.length, before: 0, after: 0 }
	);
	const shown = $derived(files.slice(range.start, range.end));
	const duration = (ms: number) => {
		const seconds = Math.round(ms / 1000);
		return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, '0')}`;
	};

	function measure() {
		if (!list) return;
		scrolledPast = -list.getBoundingClientRect().top;
		viewport = window.innerHeight;
	}
	onMount(() => {
		measure();
		const onScroll = () => requestAnimationFrame(measure);
		window.addEventListener('scroll', onScroll, { passive: true });
		window.addEventListener('resize', onScroll);
		return () => {
			window.removeEventListener('scroll', onScroll);
			window.removeEventListener('resize', onScroll);
		};
	});

	// The lightbox lives in history state, so Back closes it and the URL
	// in the address bar is the file's own, shareable page.
	const openId = $derived((page.state as { lightbox?: string }).lightbox);
	const openIndex = $derived(openId ? files.findIndex((f) => f.id === openId) : -1);
	let opener: HTMLElement | null = null;

	function open(event: MouseEvent, file: FileView) {
		if (event.metaKey || event.ctrlKey || event.shiftKey || event.button !== 0) return;
		event.preventDefault();
		opener = event.currentTarget as HTMLElement;
		pushState(resolve('/f/[id]/[[slug]]', { id: file.id }), { lightbox: file.id });
	}

	// Moving within the open lightbox replaces its history entry rather than
	// stacking one per photo: Back still closes it in one step.
	function go(index: number) {
		const file = files[index];
		if (!file) return;
		if (index >= files.length - 2 && hasMore) loadMore?.();
		replaceState(resolve('/f/[id]/[[slug]]', { id: file.id }), { lightbox: file.id });
	}

	$effect(() => {
		if (openIndex < 0 && opener) {
			const target = opener;
			opener = null;
			tick().then(() => target.focus());
		}
	});
</script>

{#if files.length === 0}
	<p class="card px-6 py-14 text-center muted">Nothing here yet.</p>
{:else}
	<div class="mb-3 flex justify-end">
		<div class="flex rounded-full bg-surface-raised p-1 text-sm font-medium" role="group" aria-label="Layout">
			{#each [['grid', 'Grid'], ['masonry', 'Masonry']] as const as [value, name] (value)}
				<button
					type="button"
					aria-pressed={prefs.layout === value}
					class="rounded-full px-4 py-1.5 {prefs.layout === value ? 'bg-surface-high text-ink' : 'muted hover:text-ink'}"
					onclick={() => prefs.setLayout(value)}>{name}</button
				>
			{/each}
		</div>
	</div>
	<!-- Edge to edge on a phone, where every pixel of picture counts. -->
	<div bind:this={list} bind:clientWidth={width} class="-mx-4 md:mx-0">
		<ul
			class={prefs.layout === 'masonry'
				? 'columns-2 gap-[3px] sm:columns-3 md:gap-2 lg:columns-4'
				: 'grid grid-cols-2 gap-[3px] sm:grid-cols-3 md:gap-2 lg:grid-cols-4'}
			style:padding-top="{range.before}px"
			style:padding-bottom="{range.after}px"
			data-testid="file-grid"
		>
			{#each shown as file (file.id)}
				<li class="relative {prefs.layout === 'masonry' ? 'mb-[3px] break-inside-avoid md:mb-2' : ''}">
					<a
						href={resolve('/f/[id]/[[slug]]', { id: file.id })}
						class="group block overflow-hidden bg-surface-raised md:rounded-xl {prefs.layout === 'grid' ? 'aspect-square' : ''}"
						onclick={(e) => open(e, file)}
						data-testid="gallery-item"
					>
						{#if thumb(file)}
							<img
								src={thumb(file)}
								alt={file.description ?? file.title ?? ''}
								loading="lazy"
								width={file.width || undefined}
								height={file.height || undefined}
								class="w-full transition-transform duration-300 group-hover:scale-[1.03] motion-reduce:transition-none {prefs.layout === 'grid' ? 'h-full object-cover' : 'h-auto'}"
							/>
						{:else}
							<span class="grid aspect-square content-center justify-items-center gap-2 p-3 text-center text-sm muted">
								<Icon name="play" size={28} />
								<span class="line-clamp-2">{file.title ?? file.kind}</span>
							</span>
						{/if}
						{#if file.kind === 'video' && file.duration_ms}
							<span class="absolute bottom-2 left-2 flex items-center gap-1 rounded-full bg-black/65 py-0.5 pr-2.5 pl-1.5 text-xs font-medium text-white tabular-nums">
								<Icon name="play" size={12} />{duration(file.duration_ms)}
							</span>
						{/if}
					</a>
					{#if selected && ontoggle}
						<label class="absolute top-2 left-2 grid size-8 place-items-center rounded-full bg-black/55">
							<span class="sr-only">Select {file.title ?? file.id}</span>
							<input
								type="checkbox"
								class="size-4 accent-[var(--color-accent)]"
								checked={selected.has(file.id)}
								data-testid="select-file"
								onchange={() => ontoggle(file.id)}
							/>
						</label>
					{/if}
				</li>
			{/each}
		</ul>
	</div>
{/if}

{#if openIndex >= 0}
	<Lightbox
		file={files[openIndex]}
		index={openIndex}
		count={files.length}
		onclose={() => history.back()}
		onmove={(to) => go(to)}
	/>
{/if}
