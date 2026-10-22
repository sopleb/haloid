<script lang="ts">
	import { onMount } from 'svelte';
	import type { WidgetMethods } from 'altcha/types';
	import { loadAltcha } from '$lib/altcha';

	/** The solved payload, or `null` until the proof-of-work is done. */
	// eslint-disable-next-line no-useless-assignment -- a $bindable prop is read by the parent, which the rule cannot see
	let { payload = $bindable() }: { payload?: string | null } = $props();
	let ready = $state(false);
	let widget = $state<HTMLElement & Partial<WidgetMethods>>();

	onMount(() => {
		loadAltcha().then(() => (ready = true));
	});

	/** A payload is single-use: after a failed submit, solve a fresh one. */
	export function reset() {
		payload = null;
		widget?.reset?.();
		widget?.verify?.();
	}
</script>

{#if ready}
	<altcha-widget
		bind:this={widget}
		challenge="/api/v2/auth/challenge"
		auto="onload"
		onstatechange={(e) => (payload = e.detail.state === 'verified' ? (e.detail.payload ?? null) : null)}
	></altcha-widget>
{/if}
