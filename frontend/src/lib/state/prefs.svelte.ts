// Per-browser display preferences. Remembered in localStorage when the
// browser allows it; a private window just forgets them.

import { parseChoice, resolveTheme, themes, type ThemeChoice } from '$lib/theme/themes';

export type Layout = 'grid' | 'masonry';

function read(key: string): string | null {
	try {
		return globalThis.localStorage?.getItem(key) ?? null;
	} catch {
		return null;
	}
}

function write(key: string, value: string) {
	try {
		globalThis.localStorage?.setItem(key, value);
	} catch {
		// Storage refused: the choice lasts until the tab closes.
	}
}

function remove(key: string) {
	try {
		globalThis.localStorage?.removeItem(key);
	} catch {
		// Nothing was stored, then.
	}
}

class Prefs {
	layout = $state<Layout>(read('haloid.layout') === 'masonry' ? 'masonry' : 'grid');
	theme = $state<ThemeChoice>(parseChoice(read('haloid.theme')));
	/** The administrator's default, once the config has arrived. */
	siteTheme = $state<string | null>(read('haloid.siteTheme'));

	setLayout(layout: Layout) {
		this.layout = layout;
		write('haloid.layout', layout);
	}

	setTheme(choice: ThemeChoice) {
		this.theme = choice;
		if (choice === 'site') remove('haloid.theme');
		else write('haloid.theme', choice);
	}

	/** Remembered so the next visit's first paint (static/theme.js) is right. */
	setSiteTheme(theme: string) {
		this.siteTheme = theme;
		write('haloid.siteTheme', theme);
	}

	/** Put the resolved theme on <html>, where app.css looks for it. */
	apply() {
		const prefersLight = globalThis.matchMedia?.('(prefers-color-scheme: light)').matches ?? false;
		const id = resolveTheme(this.theme, this.siteTheme, prefersLight);
		const root = globalThis.document?.documentElement;
		if (!root) return;
		root.dataset.theme = id;
		const meta = document.querySelector('meta[name="theme-color"]');
		meta?.setAttribute('content', themes.find((t) => t.id === id)?.scheme === 'light' ? '#f4f1ea' : '#0d0d0e');
	}
}

export const prefs = new Prefs();

/** Whether the visitor asked for less motion: no auto-playing animation. */
export function reducedMotion(): boolean {
	return globalThis.matchMedia?.('(prefers-reduced-motion: reduce)').matches ?? false;
}
