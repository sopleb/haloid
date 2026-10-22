// Sets the theme before the first paint, so a Day visitor never sees a flash
// of Night. A file rather than an inline script so a strict CSP needs no
// hash for it. Mirrors resolveTheme in src/lib/theme/themes.ts; the app
// corrects anything this gets wrong once it has the site's settings.
(function () {
	var choice = null;
	var site = null;
	try {
		choice = localStorage.getItem('haloid.theme');
		site = localStorage.getItem('haloid.siteTheme');
	} catch {
		// Storage refused: the default it is.
	}
	var theme =
		choice === 'device'
			? matchMedia('(prefers-color-scheme: light)').matches
				? 'day'
				: 'night'
			: choice || site || 'night';
	if (/^[a-z0-9-]{1,32}$/.test(theme)) document.documentElement.dataset.theme = theme;
})();
