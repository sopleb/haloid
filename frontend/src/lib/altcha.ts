// The ALTCHA widget, loaded on demand: only the sign-in and registration
// pages pay for it. The `external` build ships without inline workers or
// styles, so the strict CSP holds; the one algorithm our server issues is
// registered here, and its worker is bundled from our own origin.
let loading: Promise<void> | undefined;

export function loadAltcha(): Promise<void> {
	loading ??= (async () => {
		await import('altcha/external');
		await import('altcha/altcha.css');
		const { default: Pbkdf2Worker } = await import('altcha/workers/pbkdf2?worker');
		globalThis.$altcha.algorithms.set('PBKDF2/SHA-256', () => new Pbkdf2Worker());
	})();
	return loading;
}
