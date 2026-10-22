<script lang="ts" module>
	// Stroked 24px icons, drawn for this app. Decorative: whatever an icon
	// sits in carries the accessible name.
	const paths = {
		plus: 'M12 5v14M5 12h14',
		up: 'M12 19V6m-6 6 6-6 6 6',
		copy: 'M9 9h9a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H11a2 2 0 0 1-2-2zM5 15V6.5A2.5 2.5 0 0 1 7.5 4H15',
		check: 'm5 12.5 4.5 4.5L19 7.5',
		download: 'M12 4v11m-5-5 5 5 5-5M5 20h14',
		edit: 'M15.5 5.5l3 3L8 19H5v-3z',
		trash: 'M5 7h14M10 7V5h4v2m-7 0 1 12h8l1-12',
		link: 'M10 14a4 4 0 0 0 5.66 0l3-3a4 4 0 0 0-5.66-5.66l-1 1M14 10a4 4 0 0 0-5.66 0l-3 3a4 4 0 0 0 5.66 5.66l1-1',
		home: 'M4 11 12 4l8 7v8a1 1 0 0 1-1 1h-4v-6H9v6H5a1 1 0 0 1-1-1z',
		compass: 'M12 3.5a8.5 8.5 0 1 1 0 17 8.5 8.5 0 0 1 0-17zm3.5 5-2 5-5 2 2-5z',
		user: 'M12 4.7a3.8 3.8 0 1 1 0 7.6 3.8 3.8 0 0 1 0-7.6zM4.5 20c1.2-3.6 4-5.3 7.5-5.3s6.3 1.7 7.5 5.3',
		x: 'M6 6l12 12M18 6 6 18',
		back: 'M15 5l-7 7 7 7',
		next: 'M9 5l7 7-7 7',
		play: 'M8 5.5v13l10.5-6.5z',
		shield: 'M12 3.5 19 6v5.5c0 4.2-2.9 7.6-7 9-4.1-1.4-7-4.8-7-9V6z',
		settings: 'M12 9a3 3 0 1 1 0 6 3 3 0 0 1 0-6zM4 12h2m12 0h2M12 4v2m0 12v2M6.3 6.3l1.4 1.4m8.6 8.6 1.4 1.4m0-11.4-1.4 1.4m-8.6 8.6-1.4 1.4',
		lock: 'M7 11V8a5 5 0 0 1 10 0v3M6 11h12v9H6z',
		retry: 'M5 12a7 7 0 1 0 2-4.9M5 4v4h4'
	} as const;
	export type IconName = keyof typeof paths;
</script>

<script lang="ts">
	let { name, size = 22, class: cls = '' }: { name: IconName; size?: number; class?: string } = $props();
</script>

<svg
	class="shrink-0 {cls}"
	width={size}
	height={size}
	viewBox="0 0 24 24"
	fill={name === 'play' ? 'currentColor' : 'none'}
	stroke="currentColor"
	stroke-width="2"
	stroke-linecap="round"
	stroke-linejoin="round"
	aria-hidden="true"><path d={paths[name]} /></svg
>
