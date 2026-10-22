// The themes the app ships. Each id names a [data-theme] block in app.css;
// Night is the default and needs none.

export const themes = [
	{ id: 'night', name: 'Night', scheme: 'dark' },
	{ id: 'day', name: 'Day', scheme: 'light' }
] as const;

export type ThemeId = (typeof themes)[number]['id'];

/**
 * What a visitor picked: a theme, "device" to follow the system's light or
 * dark setting, or "site" for whatever the administrator set.
 */
export type ThemeChoice = ThemeId | 'device' | 'site';

export const DEFAULT_THEME: ThemeId = 'night';

export function isTheme(value: string | null | undefined): value is ThemeId {
	return themes.some((theme) => theme.id === value);
}

export function parseChoice(value: string | null | undefined): ThemeChoice {
	return value === 'device' || isTheme(value) ? value : 'site';
}

/**
 * The theme to show. The site's setting may name a theme this build does not
 * have (an older app, a newer server); that falls back to the default.
 * static/theme.js applies the same rule before the app loads, so the first
 * paint is already in the right colours; keep the two in step.
 */
export function resolveTheme(choice: ThemeChoice, site: string | null | undefined, prefersLight: boolean): ThemeId {
	if (choice === 'device') return prefersLight ? 'day' : 'night';
	if (choice !== 'site') return choice;
	return isTheme(site) ? site : DEFAULT_THEME;
}
