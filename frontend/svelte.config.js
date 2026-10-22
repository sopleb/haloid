import adapter from '@sveltejs/adapter-static';
import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';

/**
 * A static single-page app, served by the Rust binary: no Node process
 * beside it. Every route falls back to index.html; the server injects
 * link-preview tags into that same file for the public pages.
 *
 * @type {import('@sveltejs/kit').Config}
 */
const config = {
	preprocess: vitePreprocess(),
	kit: {
		adapter: adapter({ fallback: 'index.html' }),
		// Hashes rather than 'unsafe-inline', so a strict CSP holds.
		csp: { mode: 'hash' }
	}
};

export default config;
